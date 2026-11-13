// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::io::{SaveReducer, SaveReduction};

#[test]
fn directory_listing_is_sorted_bounded_and_granted_read_only() {
    let dir = crate::test_scratch_dir("dir-listing");
    let path = fs::canonicalize(dir.path()).unwrap();
    fs::write(path.join("z.txt"), "z").unwrap();
    fs::write(path.join(".hidden"), "h").unwrap();
    fs::create_dir(path.join("folder")).unwrap();
    let uri = path_to_file_uri(&path).unwrap();
    let admitted = admit_local_file_opts(&uri, AdmitOptions::DOCUMENT).unwrap();
    let mut grants = DocumentGrantStore::new();
    let granted = grants
        .grant_admitted(admitted, GrantAccess::ReadWrite)
        .unwrap();
    assert_eq!(granted.grant.access, GrantAccess::ReadOnly);
    let listing = granted.directory.unwrap();
    assert!(listing.text.find("folder/").unwrap() < listing.text.find(".hidden").unwrap());
    assert!(listing.text.find(".hidden").unwrap() < listing.text.find("z.txt").unwrap());
    assert_eq!(listing.entries.len(), 4);
    assert!(matches!(
        read_directory(&path, 1),
        Err(DocumentHostError::TooLarge { .. })
    ));
}

#[cfg(unix)]
#[test]
fn directory_labels_escape_control_characters_without_changing_targets() {
    let dir = crate::test_scratch_dir("dir-labels");
    let path = fs::canonicalize(dir.path()).unwrap();
    let file = path.join("line\nbreak\tname");
    fs::write(&file, "content").unwrap();
    let listing = read_directory(&path, DEFAULT_DOCUMENT_LIMIT).unwrap();
    assert!(listing.text.contains("line\\nbreak\\tname"));
    assert_eq!(
        file_uri_path(&listing.entries.last().unwrap().1).unwrap(),
        file
    );
}

/// Make `path` owner-only, as the person's own `aterm.toml` must be: the rule every reader
/// of it holds refuses one its group may write, which a plain write leaves under a
/// user-private-group login's umask `002` (Ubuntu's default).
fn owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

struct UniqueFile {
    _dir: aterm_tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for UniqueFile {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl AsRef<Path> for UniqueFile {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

fn unique_file(name: &str, bytes: &[u8]) -> UniqueFile {
    let dir = crate::test_scratch_dir(&format!("doc-host-{name}"));
    let path = dir.path().join("hello world.md");
    fs::write(&path, bytes).unwrap();
    UniqueFile { _dir: dir, path }
}

fn file_uri(path: &Path) -> String {
    path_to_file_uri(path).unwrap()
}

/// Well-formed host-absolute URI of a missing file.
const MISSING_DOC_URI: &str = if cfg!(windows) {
    "file:///C:/definitely/missing/aterm-doc.md"
} else {
    "file:///definitely/missing/aterm-doc.md"
};

/// `walk_path_components` admits a canonicalized path without stat-ing a Windows verbatim prefix.
#[test]
fn component_walk_admits_a_canonicalized_path() {
    let path = unique_file("canonical-walk", b"walk\n");
    let canonical = fs::canonicalize(&path).unwrap();
    #[cfg(windows)]
    {
        assert!(
            canonical.as_os_str().to_string_lossy().starts_with(r"\\?\"),
            "expected fs::canonicalize to produce a verbatim path, got {}",
            canonical.display()
        );
    }
    reject_symlink_components(&canonical).unwrap();
    assert!(logical_symlink_bindings(&canonical).unwrap().is_empty());
}

/// A temp-rooted document, as `normalize_process_temp_root` rewrites it, admits end to end.
#[test]
fn component_walk_admits_a_process_temporary_root_path() {
    let path = unique_file("temporary-walk", b"walk\n");
    owner_only(&path);
    let normalized = normalize_process_temp_root(&path).unwrap();
    assert!(logical_symlink_bindings(&normalized).unwrap().is_empty());
    reject_symlink_components(&normalized).unwrap();
    let contents = read_config_atomic_file(&path, 1024, false).unwrap();
    assert_eq!(contents.bytes, b"walk\n");
}

#[test]
fn local_uri_is_canonical_bounded_and_reuses_grant() {
    let path = unique_file("open", b"# hello\n");
    let mut grants = DocumentGrantStore::new();
    let uri = file_uri(&path);
    assert!(uri.contains("hello%20world.md"));
    let first = grants
        .open_local(&uri, GrantAccess::ReadOnly, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    let second = grants
        .open_local(&uri, GrantAccess::ReadWrite, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    assert_eq!(first.text, "# hello\n");
    assert_eq!(first.grant.id, second.grant.id);
    assert_eq!(second.grant.access, GrantAccess::ReadWrite);
}

/// One document opened the way a face opens one: admitted, granted for
/// writing, held in a store and registered for saving.
struct OpenDocument {
    grants: DocumentGrantStore,
    persistence: DocumentPersistenceStore,
    documents: amacs::store::DocumentStore,
    document: DocumentId,
    granted: GrantedDocument,
}

const ALLOW_MISSING: AdmitOptions = AdmitOptions {
    allow_missing: true,
    ..AdmitOptions::DOCUMENT
};

fn open_document(path: &Path, options: AdmitOptions) -> OpenDocument {
    let admitted = admit_local_file_opts(&file_uri(path), options).unwrap();
    let mut grants = DocumentGrantStore::new();
    let granted = grants
        .grant_admitted(admitted, GrantAccess::ReadWrite)
        .unwrap();
    let mut documents = crate::test_document_store();
    let document = documents.open(granted.grant.canonical_uri.clone(), granted.text.clone());
    let mut persistence = DocumentPersistenceStore::default();
    persistence
        .register(document, granted.grant.id, granted.observed, &granted.text)
        .unwrap();
    OpenDocument {
        grants,
        persistence,
        documents,
        document,
        granted,
    }
}

impl OpenDocument {
    fn text(&self) -> String {
        self.documents
            .snapshot(self.document)
            .unwrap()
            .text
            .to_string()
    }

    fn set_text(&mut self, text: &str) {
        let before = self.documents.snapshot(self.document).unwrap();
        let outcome = self.documents.transact(
            self.document,
            before.seq,
            vec![amacs::store::TextEdit {
                range: 0..before.text.len(),
                insert: text.to_string(),
            }],
        );
        assert!(
            matches!(outcome, amacs::store::DocumentTxnOutcome::Committed { .. }),
            "{outcome:?}"
        );
    }

    fn save(&mut self) -> SaveReduction {
        let snapshot = self.documents.snapshot(self.document).unwrap();
        let pending = self.persistence.begin(&snapshot).unwrap();
        let result = execute_granted_save(&self.granted.grant, &pending.plan);
        self.persistence
            .complete(self.document, pending.plan.generation, result)
            .unwrap()
    }

    fn encoding(&self) -> DiskEncoding {
        self.persistence.observed(self.document).unwrap().encoding()
    }
}

/// With `allow_missing`, a missing file admits as empty text and is created on the first save.
#[test]
fn missing_with_allow_missing_is_empty_and_first_save_creates() {
    let path = unique_file("allow-missing", b"");
    fs::remove_file(&path).unwrap();
    let admitted = admit_local_file_opts(&file_uri(&path), ALLOW_MISSING).unwrap();
    assert!(admitted.is_missing());
    assert_eq!(admitted.text, "");
    assert_eq!(admitted.encoding(), DiskEncoding::PLAIN);

    let mut opened = open_document(&path, ALLOW_MISSING);
    assert!(!opened.granted.observed.exists);
    opened.set_text("first line\n");
    let snapshot = opened.documents.snapshot(opened.document).unwrap();
    let pending = opened.persistence.begin(&snapshot).unwrap();
    assert!(
        !path.exists(),
        "admission, the grant, an edit and the save plan create nothing"
    );
    let result = execute_granted_save(&opened.granted.grant, &pending.plan);
    assert!(matches!(
        opened
            .persistence
            .complete(opened.document, pending.plan.generation, result)
            .unwrap(),
        SaveReduction::Durable(checkpoint) if checkpoint.seq == snapshot.seq
    ));
    assert_eq!(fs::read(&path).unwrap(), b"first line\n");
    assert!(opened.persistence.observed(opened.document).unwrap().exists);

    opened.set_text("second\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"second\n");
}

/// Creating the file externally after a missing-file admission causes an existence conflict on first save.
#[test]
fn first_save_refuses_if_the_file_appeared() {
    let path = unique_file("missing-then-appeared", b"");
    fs::remove_file(&path).unwrap();
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text("ours\n");
    fs::write(&path, b"theirs\n").unwrap();
    assert_eq!(
        opened.save(),
        SaveReduction::Conflict(crate::io::VersionConflict::Existence)
    );
    assert_eq!(fs::read(&path).unwrap(), b"theirs\n");
}

/// Missing files without `allow_missing` and missing parent directories with `allow_missing` are refused.
#[test]
fn missing_without_allow_missing_is_refused() {
    let path = unique_file("missing-refused", b"");
    fs::remove_file(&path).unwrap();
    let uri = file_uri(&path);
    for refused in [
        admit_local_file(&uri, DEFAULT_DOCUMENT_LIMIT, false),
        admit_local_file_opts(&uri, AdmitOptions::DOCUMENT),
        admit_local_file_opts(&uri, AdmitOptions::default()),
    ] {
        let error = refused.unwrap_err();
        assert_eq!(error.refusal_word(), "missing", "{error}");
    }

    let nested = path.parent().unwrap().join("no such folder").join("new.md");
    let error = admit_local_file_opts(&file_uri(&nested), ALLOW_MISSING).unwrap_err();
    assert_eq!(error.refusal_word(), "missing", "{error}");
    assert!(!nested.parent().unwrap().exists());
}

/// A byte order mark is not in the text, and every save puts it back.
#[test]
fn bom_round_trips_byte_exact() {
    let bytes = b"\xef\xbb\xbf# title\nbody\n";
    let path = unique_file("bom", bytes);
    let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
    assert_eq!(opened.text(), "# title\nbody\n");
    assert_eq!(
        opened.encoding(),
        DiskEncoding {
            bom: true,
            eol: crate::io::LineEnding::Lf
        }
    );
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), bytes, "a save without edits");

    opened.set_text("# title\nbody\nmore\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(
        fs::read(&path).unwrap(),
        b"\xef\xbb\xbf# title\nbody\nmore\n"
    );
    assert!(
        opened
            .persistence
            .observed_text_matches(opened.document, &opened.text())
    );
    let refreshed = opened
        .grants
        .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    assert_eq!(refreshed.text, opened.text());
}

/// A file whose every line ends in CRLF is held with LF and saved with
/// CRLF, a line typed into it included; a byte order mark beside it stays.
#[test]
fn crlf_file_round_trips_with_lf_in_buffer() {
    for (name, mark) in [("crlf", &b""[..]), ("crlf-bom", &crate::io::UTF8_BOM[..])] {
        let bytes = [mark, &b"one\r\ntwo\r\n"[..]].concat();
        let path = unique_file(name, &bytes);
        let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
        assert_eq!(opened.text(), "one\ntwo\n");
        assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
        assert_eq!(opened.encoding().bom, !mark.is_empty());
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "{name}: a save without edits"
        );

        opened.set_text("one\ntwo\nthree\n");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(
            fs::read(&path).unwrap(),
            [mark, &b"one\r\ntwo\r\nthree\r\n"[..]].concat(),
            "{name}"
        );
        let refreshed = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert_eq!(refreshed.text, "one\ntwo\nthree\n", "{name}");
        assert!(
            opened
                .persistence
                .observed_text_matches(opened.document, &refreshed.text),
            "{name}: the watch finds the buffer's own save unchanged"
        );
    }
}

/// Accepting an overwrite rebinds the baseline while preserving the buffer's line-ending mapping.
#[test]
fn an_overwrite_keeps_the_buffers_mapping() {
    for (name, read, theirs, ours, written) in [
        (
            "overwrite-crlf",
            &b"one\r\ntwo\r\n"[..],
            &b"theirs\n"[..],
            "ours\n",
            &b"ours\r\n"[..],
        ),
        (
            "overwrite-mixed",
            b"a\r\nb\n",
            b"x\r\ny\r\n",
            "a\r\nb\nc\n",
            b"a\r\nb\nc\n",
        ),
    ] {
        let path = unique_file(name, read);
        let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
        opened.set_text(ours);
        fs::write(&path, theirs).unwrap();
        assert!(
            matches!(opened.save(), SaveReduction::Conflict(_)),
            "{name}"
        );
        let snapshot = opened.documents.snapshot(opened.document).unwrap();
        assert!(matches!(
            opened.persistence.begin(&snapshot),
            Err(PersistenceError::Save(SaveError::ConflictRequiresReconcile))
        ));
        assert_eq!(fs::read(&path).unwrap(), theirs, "{name}: nothing written");

        let fresh = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        opened
            .persistence
            .accept_overwrite(opened.document, fresh.observed)
            .unwrap();
        assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
        assert_eq!(fs::read(&path).unwrap(), written, "{name}");
    }
}

/// One line feed without a CR before it keeps the whole file as it is: its
/// CRs stay in the text, and saves write the text's own bytes.
#[test]
fn mixed_eol_is_byte_exact() {
    for (name, bytes) in [
        ("mixed", &b"a\r\nb\nc\r\n"[..]),
        ("leading-lf", b"\na\r\nb\r\n"),
        ("bare-cr", b"a\rb\r"),
        ("no-break", b"a\r"),
    ] {
        let path = unique_file(name, bytes);
        let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
        assert_eq!(opened.text().as_bytes(), bytes, "{name}");
        assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), bytes, "{name}");

        let edited = format!("{}x\n", opened.text());
        opened.set_text(&edited);
        assert!(matches!(opened.save(), SaveReduction::Durable(_)));
        assert_eq!(fs::read(&path).unwrap(), edited.as_bytes(), "{name}");
    }
}

/// The mapping is the buffer's, not a fresh reading of each save's bytes.
#[test]
fn a_document_keeps_the_mapping_it_was_read_with_across_saves() {
    let path = unique_file("mixed-becomes-crlf", b"a\r\nb\n");
    let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
    opened.set_text("a\r\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"a\r\n");
    assert_eq!(opened.encoding(), DiskEncoding::PLAIN);
    opened.set_text("a\r\nc\r\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"a\r\nc\r\n");

    let path = unique_file("crlf-without-break", b"a\r\nb\r\n");
    let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
    opened.set_text("ab");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"ab");
    let refreshed = opened
        .grants
        .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    assert_eq!(refreshed.observed.encoding(), DiskEncoding::PLAIN);
    opened
        .persistence
        .accept_observation(opened.document, refreshed.observed, &refreshed.text)
        .unwrap();
    assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
    opened.set_text("a\nb\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\r\n");

    fs::write(&path, b"x\ny\n").unwrap();
    let refreshed = opened
        .grants
        .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    opened.set_text(&refreshed.text);
    opened
        .persistence
        .accept_observation(opened.document, refreshed.observed, &refreshed.text)
        .unwrap();
    assert_eq!(opened.encoding(), DiskEncoding::PLAIN);
    opened.set_text("x\ny\nz\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"x\ny\nz\n");
}

/// A buffer held as it is can save bytes that a fresh read takes another
/// way: every break CRLF reads back with LF, and a leading U+FEFF reads
/// back as a mark.
#[test]
fn a_save_read_back_another_way_is_unchanged() {
    for (name, start, held, reading, encoding) in [
        (
            "new-crlf",
            None,
            "x\r\ny\r\n",
            "x\ny\n",
            DiskEncoding {
                bom: false,
                eol: crate::io::LineEnding::CrLf,
            },
        ),
        (
            "mixed-to-crlf",
            Some(&b"a\r\nb\n"[..]),
            "a\r\n",
            "a\n",
            DiskEncoding {
                bom: false,
                eol: crate::io::LineEnding::CrLf,
            },
        ),
        (
            "leading-feff",
            None,
            "\u{feff}hi\n",
            "hi\n",
            DiskEncoding {
                bom: true,
                eol: crate::io::LineEnding::Lf,
            },
        ),
    ] {
        let path = unique_file(name, start.unwrap_or(b""));
        let options = if start.is_none() {
            fs::remove_file(&path).unwrap();
            ALLOW_MISSING
        } else {
            AdmitOptions::DOCUMENT
        };
        let mut opened = open_document(&path, options);
        opened.set_text(held);
        assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
        assert_eq!(fs::read(&path).unwrap(), held.as_bytes(), "{name}");
        assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");

        let refreshed = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        assert_eq!(refreshed.text, reading, "{name}: the fresh read");
        assert_eq!(refreshed.observed.encoding(), encoding, "{name}");
        let baseline = opened.persistence.observed(opened.document).unwrap();
        assert_eq!(
            crate::io::detect_version_conflict(baseline, refreshed.observed),
            None,
            "{name}"
        );
        let document = opened.document;
        for text in [held, reading] {
            assert!(
                opened.persistence.observed_text_matches(document, text),
                "{name}: {text:?}"
            );
            assert!(
                opened
                    .persistence
                    .last_observed_text_matches(document, text),
                "{name}: {text:?}"
            );
        }
        assert!(
            !opened
                .persistence
                .observed_text_matches(document, &format!("{reading}z")),
            "{name}: another text is still a change"
        );
        assert_eq!(
            opened.persistence.observed_text_fingerprint(document),
            Some(ContentFingerprint::of(reading.as_bytes())),
            "{name}: a reopen admits the fresh reading"
        );

        opened
            .persistence
            .accept_observation(document, refreshed.observed, &refreshed.text)
            .unwrap();
        assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");
        assert!(
            opened.persistence.observed_text_matches(document, held),
            "{name}"
        );
        assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
        assert_eq!(
            fs::read(&path).unwrap(),
            held.as_bytes(),
            "{name}: the second save"
        );

        fs::remove_file(&path).unwrap();
        fs::write(&path, held.as_bytes()).unwrap();
        let rewritten = opened
            .grants
            .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap();
        let baseline = opened.persistence.observed(document).unwrap();
        assert!(
            crate::io::content_is_equivalent(baseline, rewritten.observed),
            "{name}"
        );
        assert!(
            opened
                .persistence
                .last_observed_text_matches(document, &rewritten.text),
            "{name}"
        );
        opened
            .persistence
            .accept_observation(document, rewritten.observed, &rewritten.text)
            .unwrap();
        assert_eq!(opened.encoding(), DiskEncoding::PLAIN, "{name}");
        opened.set_text(&format!("{held}z"));
        assert!(matches!(opened.save(), SaveReduction::Durable(_)), "{name}");
        assert_eq!(
            fs::read(&path).unwrap(),
            format!("{held}z").as_bytes(),
            "{name}: an edit after the rebind"
        );
    }
}

/// Reloading adopts the fresh reading's encoding unless it is equivalent to the buffer's existing reading.
#[test]
fn a_reload_takes_the_fresh_reading_and_its_mapping() {
    let path = unique_file("reload-crlf", b"");
    fs::remove_file(&path).unwrap();
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text("x\r\ny\r\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    opened.set_text("x\r\ny\r\nlocal edit");
    let refreshed = opened
        .grants
        .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    opened.set_text(&refreshed.text);
    opened
        .persistence
        .accept_reloaded_observation(opened.document, refreshed.observed, &refreshed.text)
        .unwrap();
    assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
    assert!(
        opened
            .persistence
            .observed_text_matches(opened.document, "x\ny\n")
    );
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"x\r\ny\r\n");
    opened.set_text("x\ny\nz\n");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    assert_eq!(fs::read(&path).unwrap(), b"x\r\ny\r\nz\r\n");

    let path = unique_file("reload-crlf-no-break", b"a\r\nb\r\n");
    let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
    opened.set_text("ab");
    assert!(matches!(opened.save(), SaveReduction::Durable(_)));
    let refreshed = opened
        .grants
        .refresh_local(opened.granted.grant.id, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    opened
        .persistence
        .accept_reloaded_observation(opened.document, refreshed.observed, &refreshed.text)
        .unwrap();
    assert_eq!(opened.encoding().eol, crate::io::LineEnding::CrLf);
}

/// The draft-journal fingerprint is the admitted text's, which is not the
/// file's own when admission took a mark or CRs off.
#[test]
fn the_observed_text_fingerprint_is_the_admitted_texts() {
    let bytes = b"\xef\xbb\xbfone\r\ntwo\r\n";
    let path = unique_file("text-fingerprint", bytes);
    let opened = open_document(&path, AdmitOptions::DOCUMENT);
    let observed = opened.persistence.observed(opened.document).unwrap();
    assert_eq!(observed.content, ContentFingerprint::of(bytes));
    assert_eq!(
        opened
            .persistence
            .observed_text_fingerprint(opened.document),
        Some(ContentFingerprint::of(b"one\ntwo\n"))
    );

    let path = unique_file("text-fingerprint-missing", b"");
    fs::remove_file(&path).unwrap();
    let opened = open_document(&path, ALLOW_MISSING);
    assert_eq!(
        opened
            .persistence
            .observed_text_fingerprint(opened.document),
        None,
        "no file, no image"
    );
}

/// Document saves never create missing parent directories.
#[test]
fn a_document_save_makes_no_folder() {
    let root = unique_file("no-folder", b"");
    fs::remove_file(&root).unwrap();
    let dir = root.parent().unwrap().join("sub");
    fs::create_dir(&dir).unwrap();

    let missing = dir.join("new.md");
    let mut opened = open_document(&missing, ALLOW_MISSING);
    opened.set_text("text\n");
    fs::remove_dir(&dir).unwrap();
    let reduction = opened.save();
    assert!(
        matches!(
            &reduction,
            SaveReduction::Failed {
                stage: AtomicSaveStage::CreateTemporary,
                message,
                cause: Some(SaveCause {
                    kind: std::io::ErrorKind::NotFound,
                    touch: SaveTouch::Folder,
                }),
            } if message.ends_with(MISSING_SUFFIX)
        ),
        "{reduction:?}"
    );
    assert!(!dir.exists(), "the first save made no folder");

    fs::create_dir(&dir).unwrap();
    let saved = dir.join("saved.md");
    fs::write(&saved, b"saved\n").unwrap();
    let mut opened = open_document(&saved, AdmitOptions::DOCUMENT);
    opened.set_text("edited\n");
    fs::remove_file(&saved).unwrap();
    fs::remove_dir(&dir).unwrap();
    assert_eq!(
        opened.save(),
        SaveReduction::Conflict(crate::io::VersionConflict::Existence)
    );
    assert!(!dir.exists(), "a save made no folder");
}

/// A never-saved document's file is still absent on a refresh: no change,
/// through the missing-baseline refresh.
#[test]
fn a_missing_document_refreshes_as_missing_until_its_file_appears() {
    let path = unique_file("missing-refresh", b"");
    fs::remove_file(&path).unwrap();
    let opened = open_document(&path, ALLOW_MISSING);
    let id = opened.granted.grant.id;
    assert_eq!(
        opened
            .grants
            .refresh_local(id, DEFAULT_DOCUMENT_LIMIT)
            .unwrap_err()
            .refusal_word(),
        "missing"
    );
    let absent = opened
        .grants
        .refresh_local_allowing_missing(id, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    assert!(!absent.observed.exists);
    assert_eq!(absent.text, "");
    assert_eq!(
        crate::io::detect_version_conflict(opened.granted.observed, absent.observed),
        None
    );

    fs::write(&path, b"from elsewhere\r\n").unwrap();
    let appeared = opened
        .grants
        .refresh_local_allowing_missing(id, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    assert!(appeared.observed.exists);
    assert_eq!(appeared.text, "from elsewhere\n");
    assert_eq!(
        crate::io::detect_version_conflict(opened.granted.observed, appeared.observed),
        Some(crate::io::VersionConflict::Existence)
    );
}

/// `open_path` resolves symlinked parent directories for both existing and missing files.
#[cfg(unix)]
#[test]
fn open_path_resolves_a_symlinked_parent() {
    use std::os::unix::fs::symlink;

    let path = unique_file("open-path", b"");
    let dir = path.parent().unwrap();
    let real = dir.join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("present.md"), b"here\n").unwrap();
    let link = dir.join("link");
    symlink(&real, &link).unwrap();
    let canonical = fs::canonicalize(&real).unwrap();

    let (present, uri) = open_path(&link.join("present.md")).unwrap();
    assert_eq!(present, canonical.join("present.md"));
    assert_eq!(uri, path_to_file_uri(&present).unwrap());
    assert_eq!(
        admit_local_file_opts(&uri, AdmitOptions::DOCUMENT)
            .unwrap()
            .text,
        "here\n"
    );

    let (missing, uri) = open_path(&link.join("new file.md")).unwrap();
    assert_eq!(missing, canonical.join("new file.md"));
    assert!(uri.ends_with("/real/new%20file.md"), "{uri}");
    assert!(
        admit_local_file_opts(&uri, ALLOW_MISSING)
            .unwrap()
            .is_missing()
    );
    assert!(matches!(
        admit_local_file_opts(&file_uri(&link.join("new file.md")), ALLOW_MISSING),
        Err(DocumentHostError::SymlinkComponent { .. })
    ));

    symlink(dir.join("nowhere"), dir.join("dangling")).unwrap();
    assert!(matches!(
        open_path(&dir.join("dangling")),
        Err(DocumentHostError::SymlinkComponent { .. })
    ));
    assert_eq!(
        open_path(&dir.join("no such folder").join("x.md"))
            .unwrap_err()
            .refusal_word(),
        "missing"
    );

    #[cfg(target_os = "macos")]
    {
        let name = format!("aterm-open-path-{}-absent.md", std::process::id());
        let (resolved, _) = open_path(&Path::new("/tmp").join(&name)).unwrap();
        assert_eq!(resolved, Path::new("/private/tmp").join(&name));
    }
}

/// `EDEADLK` from `read(2)` on an evicted dataless file maps to `DocumentHostError::NotDownloaded`.
#[cfg(unix)]
#[test]
fn edeadlk_on_read_is_reported_as_not_downloaded() {
    let path = Path::new("/Users//someone/Library/Mobile Documents/notes.md");
    let evicted = read_error(path, std::io::Error::from_raw_os_error(libc::EDEADLK));
    assert_eq!(
        evicted,
        DocumentHostError::NotDownloaded {
            path: path.to_path_buf()
        }
    );

    let other = read_error(path, std::io::Error::from_raw_os_error(libc::EIO));
    assert!(
        matches!(
            &other,
            DocumentHostError::Io {
                stage: AtomicSaveStage::Preflight,
                ..
            }
        ),
        "{other:?}"
    );
}

/// Admission then grant composes to the synchronous open: admission is a pure function
/// of the URI, granting is the store's only mutation, and admitting twice mints one grant.
#[test]
fn admission_then_grant_matches_the_synchronous_open() {
    let path = unique_file("two-phase", b"# split\n");
    let uri = file_uri(&path);
    let admitted = admit_local_file(&uri, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
    assert_eq!(admitted.text, "# split\n");
    assert!(admitted.canonical_uri.starts_with("file:///"));
    assert!(admitted.path.is_absolute());

    let mut grants = DocumentGrantStore::new();
    let first = grants
        .grant_admitted(admitted.clone(), GrantAccess::ReadOnly)
        .unwrap();
    let second = grants
        .grant_admitted(admitted, GrantAccess::ReadWrite)
        .unwrap();
    assert_eq!(first.grant.id, second.grant.id);
    assert_eq!(second.grant.access, GrantAccess::ReadWrite);
    assert_eq!(first.text, "# split\n");
    assert_eq!(first.observed, second.observed);

    let direct = grants
        .open_local(&uri, GrantAccess::ReadOnly, DEFAULT_DOCUMENT_LIMIT)
        .unwrap();
    assert_eq!(direct.grant.id, first.grant.id);
    assert_eq!(direct.grant.canonical_uri, first.grant.canonical_uri);
    assert!(
        matches!(
            admit_local_file(MISSING_DOC_URI, 16, false),
            Err(DocumentHostError::Io { .. })
        ),
        "a missing file fails at admission, before any grant state exists"
    );
}

#[test]
fn malformed_remote_binary_and_oversize_inputs_fail_closed() {
    let mut grants = DocumentGrantStore::new();
    assert!(matches!(
        grants.open_local("https://example.com/a", GrantAccess::ReadOnly, 10),
        Err(DocumentHostError::UnsupportedScheme)
    ));
    assert!(matches!(
        grants.open_local("file://example.com/a", GrantAccess::ReadOnly, 10),
        Err(DocumentHostError::RemoteAuthority)
    ));
    assert!(matches!(
        grants.open_local("file:///tmp/%zz", GrantAccess::ReadOnly, 10),
        Err(DocumentHostError::InvalidEncoding)
    ));
    let path = unique_file("invalid", &[0xff]);
    assert!(matches!(
        grants.open_local(&file_uri(&path), GrantAccess::ReadOnly, 10),
        Err(DocumentHostError::InvalidUtf8)
    ));
    fs::write(&path, b"too long").unwrap();
    assert!(matches!(
        grants.open_local(&file_uri(&path), GrantAccess::ReadOnly, 2),
        Err(DocumentHostError::TooLarge { limit: 2 })
    ));
}

/// `decode_utf8` strips at most one leading UTF-8 BOM and validates the remaining bytes.
#[test]
fn decoding_takes_off_one_leading_bom_and_still_checks_the_rest() {
    let decode = |bytes: &[u8]| decode_utf8(bytes.to_vec()).map(|decoded| decoded.text);
    assert_eq!(decode(b"\xef\xbb\xbfhi").unwrap(), "hi");
    assert_eq!(decode(b"\xef\xbb\xbf").unwrap(), "");
    assert_eq!(
        decode(b"\xef\xbb\xbf\xef\xbb\xbf").unwrap(),
        "\u{feff}",
        "only the first BOM is taken off"
    );
    assert_eq!(decode(b"hi\xef\xbb\xbf").unwrap(), "hi\u{feff}");
    assert_eq!(decode(b"").unwrap(), "");
    assert!(matches!(
        decode(b"\xef\xbb"),
        Err(DocumentHostError::InvalidUtf8)
    ));
    assert!(matches!(
        decode(b"\xef\xbb\xbf\xff"),
        Err(DocumentHostError::InvalidUtf8)
    ));
}

/// A budget the clock cannot add to now is no ceiling: the wait neither
/// panics working out its deadline nor gives up while the lock is held.
#[test]
fn a_lock_budget_past_the_clock_waits_without_a_ceiling() {
    let path = unique_file("unbounded-lock-budget", b"");
    let open = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap()
    };
    let Ok(held) = acquire_advisory_lock_within(open(), std::time::Duration::MAX) else {
        panic!("an uncontended lock is taken at once");
    };

    let (sent, outcome) = std::sync::mpsc::channel();
    let contender = open();
    let waiter = std::thread::spawn(move || {
        let taken = acquire_advisory_lock_within(contender, std::time::Duration::MAX);
        let _ = sent.send(taken.is_ok());
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    drop(held);
    assert_eq!(
        outcome.recv_timeout(std::time::Duration::from_secs(60)),
        Ok(true),
        "the unbounded wait takes the lock once it is let go"
    );
    waiter.join().unwrap();
}

#[cfg(unix)]
#[test]
fn save_lock_refuses_fifo_and_symlink_without_touching_the_target() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::symlink;

    let path = unique_file("special-save-lock", b"before");
    let contents = read_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
    let lock_path = path.parent().unwrap().join(format!(
        ".{}.aterm-write.lock",
        path.file_name().unwrap().to_string_lossy()
    ));
    let lock_c =
        std::ffi::CString::new(lock_path.as_os_str().as_bytes()).expect("save-lock FIFO path");
    // SAFETY: `lock_c` is a live NUL-terminated pathname and `mkfifo` retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(lock_c.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(matches!(
        commit_atomic_bytes(&contents.baseline, b"after"),
        AtomicCommitResult::Failed {
            stage: AtomicSaveStage::Preflight,
            message,
            cause: None,
        } if message.contains("regular non-link")
    ));
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the FIFO save lock parked the commit"
    );
    assert_eq!(fs::read(&path).unwrap(), b"before");

    fs::remove_file(&lock_path).unwrap();
    let victim = path.parent().unwrap().join("lock-victim");
    fs::write(&victim, b"victim").unwrap();
    symlink(&victim, &lock_path).unwrap();
    assert!(matches!(
        commit_atomic_bytes(&contents.baseline, b"after"),
        AtomicCommitResult::Failed {
            stage: AtomicSaveStage::Preflight,
            ..
        }
    ));
    assert_eq!(fs::read(&path).unwrap(), b"before");
    assert_eq!(fs::read(&victim).unwrap(), b"victim");
    assert!(
        fs::symlink_metadata(&lock_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// A required writer outlives optional journal patience, and still has a hang detector.
#[test]
fn a_transient_save_lock_holder_is_ridden_out_instead_of_refusing_the_save() {
    let path = unique_file("transient-save-lock", b"before");
    let lock_path = path.parent().unwrap().join(format!(
        ".{}.aterm-write.lock",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut held = Some(HeldAdvisoryLock::lock(open_write_lock(&lock_path).unwrap()).unwrap());
    let probe = open_write_lock(&lock_path).unwrap();
    let next = acquire_write_lock_after(
        probe,
        std::time::Duration::from_millis(600),
        |file, left| {
            let (file, why) = HeldAdvisoryLock::try_lock_keeping(file)
                .expect_err("CONTROL: the native writer still holds it");
            assert!(matches!(why, std::fs::TryLockError::WouldBlock));
            drop(held.take());
            acquire_write_lock_within(file, left)
        },
    )
    .expect("the required save can follow its scheduled sibling writer");
    assert!(held.is_none());
    let expired = acquire_write_lock_after(
        open_write_lock(&lock_path).unwrap(),
        std::time::Duration::from_secs(61),
        |_, _| panic!("no acquisition past the hang detector"),
    )
    .expect_err("the exhausted hang detector preserves the other writer");
    assert_eq!(
        expired.cause,
        Some(SaveCause {
            touch: SaveTouch::Lock,
            kind: std::io::ErrorKind::WouldBlock
        })
    );
    assert!(matches!(
        HeldAdvisoryLock::try_lock(open_write_lock(&lock_path).unwrap()),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert_eq!(fs::read(&path).unwrap(), b"before");
    drop(next);
}

#[test]
fn held_save_lock_returns_busy_and_retry_commits() {
    let path = unique_file("held-save-lock", b"before");
    let contents = read_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
    let lock_path = path.parent().unwrap().join(format!(
        ".{}.aterm-write.lock",
        path.file_name().unwrap().to_string_lossy()
    ));
    let held = HeldAdvisoryLock::lock(open_write_lock(&lock_path).unwrap()).unwrap();

    assert!(matches!(
        commit_atomic_bytes_with_lock_elapsed(&contents.baseline, b"after", next_temporary_seed(), Flush::Durable, Directories::Create, std::time::Duration::from_secs(61)),
        AtomicCommitResult::Failed {
            stage: AtomicSaveStage::Preflight,
            message,
            cause: Some(SaveCause {
                kind: std::io::ErrorKind::WouldBlock,
                touch: SaveTouch::Lock,
            }),
        } if message.contains("busy") && message.contains("retry Save")
    ));
    assert_eq!(fs::read(&path).unwrap(), b"before");

    drop(held);
    assert!(matches!(
        commit_atomic_bytes(&contents.baseline, b"after"),
        AtomicCommitResult::Committed(_)
    ));
    assert_eq!(fs::read(&path).unwrap(), b"after");
}

/// The save lock is free the instant its hold drops, even while a copy of the holder's
/// description lives (standing in for a shell forked mid-save).
#[test]
fn a_released_save_lock_is_free_while_a_forked_copy_of_it_lives() {
    let path = unique_file("save-lock-forked-copy", b"before");
    let contents = read_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, false).unwrap();
    let lock_path = path.parent().unwrap().join(format!(
        ".{}.aterm-write.lock",
        path.file_name().unwrap().to_string_lossy()
    ));

    let Ok((held, _, _)) = lock_and_verify_target(
        &contents.baseline,
        Directories::Create,
        std::time::Duration::ZERO,
    ) else {
        panic!("an uncontended preflight takes the save lock");
    };
    let forked_copy = held.0.try_clone().unwrap();
    drop(held);

    let next = HeldAdvisoryLock::try_lock(open_write_lock(&lock_path).unwrap());
    assert!(
        next.is_ok(),
        "the save lock outlived its hold on the forked copy of its description"
    );
    drop(next);
    drop(forked_copy);
}

/// Documents under `$TMPDIR` resolve cleanly across macOS's `/var` compatibility symlink.
#[test]
fn temporary_root_document_resolves_despite_the_var_compatibility_symlink() {
    let path = unique_file("temp-root-normalize", b"body");
    let target = resolve_atomic_target(&path, false)
        .expect("a document under the process temporary root must be admissible");
    let canonical_root = fs::canonicalize(std::env::temp_dir()).unwrap();
    assert!(
        target.target_path().starts_with(&canonical_root),
        "target {:?} must sit under the canonical temp root {:?}",
        target.target_path(),
        canonical_root
    );
    assert_eq!(target.logical_path(), path.as_path());
}

/// A path outside the temporary root claims no symlink exemption, so the rewrite
/// leaves it untouched.
#[test]
fn paths_outside_the_temporary_root_are_not_rewritten() {
    let outside = PathBuf::from("/usr/share/aterm-does-not-exist/doc.md");
    assert_eq!(
        normalize_process_temp_root(&outside).unwrap(),
        outside,
        "a non-temporary path must be returned verbatim"
    );
}

/// `aterm.toml` is read by the one rule the `[swarm]` switch's readers hold: this user's own
/// file, or root's (home-manager's read-only store file a symlink names), with no group or
/// other write; a document read holds no such rule.
/// NEGATIVE CONTROL: read the config with `Admit::Any`: the group-writable file is read.
#[cfg(unix)]
#[test]
fn aterm_toml_reads_hold_the_rule_every_reader_holds() {
    use std::os::unix::fs::PermissionsExt as _;
    let path = unique_file("config-rule", b"[swarm]\nenabled = false\n");
    owner_only(&path);
    assert!(
        read_config_atomic_file(&path, 1024, false).is_ok(),
        "this user's own"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
    assert!(
        matches!(
            read_config_atomic_file(&path, 1024, false),
            Err(DocumentHostError::Io {
                kind: Some(std::io::ErrorKind::PermissionDenied),
                ..
            })
        ),
        "its group may write it"
    );
    assert!(
        read_atomic_file(&path, 1024, false).is_ok(),
        "a document read holds no such rule"
    );
    let store = Path::new("/private/etc/hosts.equiv");
    if let Ok(meta) = fs::metadata(store) {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() == 0 && meta.mode() & 0o022 == 0 {
            let linked = path.with_file_name("aterm.toml");
            std::os::unix::fs::symlink(store, &linked).unwrap();
            assert!(
                read_config_atomic_file(&linked, 1024 * 1024, false).is_ok(),
                "root's read-only file a symlink names"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn canonical_final_target_symlink_swap_is_refused_by_the_opened_handle_seam() {
    use std::os::unix::fs::symlink;

    let path = unique_file("canonical-final-link", b"original");
    let displaced = path.with_extension("displaced");
    let target = resolve_atomic_target(&path, false).unwrap();
    fs::rename(&path, &displaced).unwrap();
    symlink(&displaced, &path).unwrap();

    assert!(matches!(
        read_stable_file(target.target_path(), 1024, false, Admit::Any),
        Err(DocumentHostError::SymlinkComponent { path: rejected, .. })
            if rejected == target.target_path()
    ));
    assert_eq!(fs::read(&displaced).unwrap(), b"original");
}

#[test]
fn save_preflight_bounds_external_growth_and_rejects_oversized_desired_bytes() {
    let path = unique_file("bounded-save-preflight", b"old");
    let contents = read_atomic_file(&path, 1024, false).unwrap();
    fs::write(&path, b"theirs").unwrap();
    let grown_len = (MAX_ATOMIC_SAVE_BYTES as u64).saturating_mul(4);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(grown_len)
        .unwrap();

    let sampled = observe_file(&path, contents.baseline.observed.len as usize).unwrap();
    assert_eq!(sampled.len, grown_len);
    assert_eq!(sampled.content, ContentFingerprint::of(b"thei"));
    assert!(matches!(
        crate::io::reduce_file_watch(crate::io::FileWatchInput {
            baseline: contents.baseline.observed,
            observed: sampled,
            document_dirty: true,
            save_in_flight: false,
        }),
        crate::io::FileWatchReduction::ConflictDirty {
            change: crate::io::VersionConflict::Content,
        }
    ));
    assert!(matches!(
        commit_atomic_bytes(&contents.baseline, b"ours"),
        AtomicCommitResult::Conflict { observed, .. } if observed.len == grown_len
    ));

    fs::write(&path, b"old").unwrap();
    let fresh = read_atomic_file(&path, 1024, false).unwrap();
    let oversized = vec![b'x'; MAX_ATOMIC_SAVE_BYTES + 1];
    assert!(matches!(
        commit_atomic_bytes(&fresh.baseline, &oversized),
        AtomicCommitResult::Failed {
            stage: AtomicSaveStage::Preflight,
            message,
            cause: Some(SaveCause {
                kind: std::io::ErrorKind::FileTooLarge,
                ..
            }),
        } if message.contains("document limit")
    ));
    assert_eq!(fs::read(&path).unwrap(), b"old");
}

#[test]
fn windows_drive_file_uri_text_round_trips_without_a_posix_root() {
    assert_eq!(
        windows_file_uri_path_text("/C:/Users//Ada/My File.toml").unwrap(),
        "C:/Users//Ada/My File.toml"
    );
    assert_eq!(windows_file_uri_path_text("/z:/").unwrap(), "z:/");
    assert!(matches!(
        windows_file_uri_path_text("//server/share/aterm.toml"),
        Err(DocumentHostError::RemoteAuthority)
    ));
}

#[cfg(windows)]
#[test]
fn windows_drive_path_and_file_uri_round_trip_through_real_path_rules() {
    let path = Path::new(r"C:\Users\Ada\My File.toml");
    let uri = path_to_file_uri(path).unwrap();
    assert_eq!(uri, "file:///C:/Users//Ada/My%20File.toml");
    assert_eq!(file_uri_path(&uri).unwrap(), path);
}

#[test]
fn validation_io_is_not_mislabeled_as_a_disk_conflict() {
    let path = unique_file("validation-class", b"old");
    let target = read_atomic_file(&path, 1024, false)
        .unwrap()
        .baseline
        .target;
    assert!(matches!(
        atomic_validation_failure(
            &target,
            AtomicSaveStage::Preflight,
            DocumentHostError::Io {
                stage: AtomicSaveStage::Preflight,
                kind: Some(std::io::ErrorKind::PermissionDenied),
                message: "permission denied".to_string(),
            },
        ),
        AtomicCommitResult::Failed {
            stage: AtomicSaveStage::Preflight,
            message,
            cause: Some(SaveCause {
                kind: std::io::ErrorKind::PermissionDenied,
                touch: SaveTouch::Read,
            }),
        } if message.contains("permission denied")
    ));
    assert!(matches!(
        atomic_validation_failure(
            &target,
            AtomicSaveStage::Preflight,
            DocumentHostError::TargetRetargeted,
        ),
        AtomicCommitResult::Conflict { .. }
    ));
}

#[test]
fn atomic_save_proof_is_accepted_by_reducer() {
    let path = unique_file("save", b"old");
    let mut grants = DocumentGrantStore::new();
    let opened = grants
        .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
        .unwrap();
    let mut documents = crate::test_document_store();
    let document = documents.open(opened.grant.canonical_uri.clone(), "new".to_string());
    let snapshot = documents.snapshot(document).unwrap();
    let mut reducer = SaveReducer::new(document, opened.observed);
    let plan = reducer.begin(&snapshot).unwrap();
    let result = grants.execute_save(opened.grant.id, &plan);
    let reduction = reducer.complete(plan.generation, result);
    assert!(
        matches!(reduction, SaveReduction::Durable(checkpoint) if checkpoint.seq == snapshot.seq)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp")
    }));
}

#[test]
fn atomic_commit_creates_a_missing_target() {
    let path = unique_file("missing-target", b"remove me");
    fs::remove_file(&path).unwrap();
    let contents = read_atomic_file(&path, 1024, true).unwrap();
    assert!(contents.bytes.is_empty());
    assert!(!contents.baseline.observed.exists);

    let proof = match commit_atomic_bytes(&contents.baseline, b"theme = \"Nord\"\n") {
        AtomicCommitResult::Committed(proof) => proof,
        other => panic!("missing-target commit failed: {other:?}"),
    };
    assert!(proof.observed.exists);
    assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Nord\"\n");
}

/// A save the person makes is durable, file and entry; the empty config file a
/// missing one is created as is only ordered (aterm-durable's counting seam).
#[test]
fn a_save_is_durable_and_a_created_config_file_only_ordered() {
    use aterm_durable::seam::Recording;
    let path = unique_file("flush-tiers", b"old");
    let contents = read_atomic_file(&path, 1024, true).unwrap();
    let recording = Recording::start();
    assert!(matches!(
        commit_atomic_bytes(&contents.baseline, b"saved"),
        AtomicCommitResult::Committed(_)
    ));
    let saved = recording.take();
    #[cfg(unix)]
    let expected_save = [Flush::Durable, Flush::Durable];
    #[cfg(not(unix))]
    let expected_save = [Flush::Durable];
    assert_eq!(saved, expected_save, "a save");
    fs::remove_file(&path).unwrap();
    let missing = read_atomic_file(&path, 1024, true).unwrap();
    let _ = recording.take();
    assert!(matches!(
        commit_atomic_bytes_ordered(&missing.baseline, b""),
        AtomicCommitResult::Committed(_)
    ));
    #[cfg(unix)]
    let expected_create = [Flush::Ordered, Flush::Ordered];
    #[cfg(not(unix))]
    let expected_create = [Flush::Ordered];
    assert_eq!(recording.take(), expected_create, "a created config file");
}

#[test]
fn external_change_returns_conflict_without_overwrite() {
    let path = unique_file("conflict", b"old");
    let mut grants = DocumentGrantStore::new();
    let opened = grants
        .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
        .unwrap();
    let mut documents = crate::test_document_store();
    let document = documents.open(opened.grant.canonical_uri.clone(), "ours".to_string());
    let mut reducer = SaveReducer::new(document, opened.observed);
    let plan = reducer
        .begin(&documents.snapshot(document).unwrap())
        .unwrap();
    fs::write(&path, "theirs").unwrap();
    assert!(matches!(
        grants.execute_save(opened.grant.id, &plan),
        AtomicSaveResult::Conflict { .. }
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "theirs");
}

#[cfg(unix)]
#[test]
fn regular_target_atomic_replace_with_identical_bytes_keeps_grant_valid() {
    let path = unique_file("equivalent-generation", b"same bytes\n");
    let replacement = path.with_extension("replacement");
    let mut grants = DocumentGrantStore::new();
    let opened = grants
        .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
        .unwrap();
    fs::write(&replacement, b"same bytes\n").unwrap();
    fs::rename(&replacement, &path).unwrap();

    let refreshed = grants.refresh_local(opened.grant.id, 1024).unwrap();
    assert_eq!(refreshed.text, opened.text);
    assert_eq!(
        crate::io::detect_version_conflict(opened.observed, refreshed.observed),
        Some(crate::io::VersionConflict::Identity),
        "atomic replacement advances the regular-file identity even when bytes match"
    );
    refreshed.grant.validate_current_binding().unwrap();
}

#[test]
fn persistence_completion_is_document_owned_not_view_owned() {
    let path = unique_file("document-owner", b"old");
    let mut grants = DocumentGrantStore::new();
    let opened = grants
        .open_local(&file_uri(&path), GrantAccess::ReadWrite, 1024)
        .unwrap();
    let mut documents = crate::test_document_store();
    let document = documents.open(opened.grant.canonical_uri.clone(), "saved".to_string());
    let snapshot = documents.snapshot(document).unwrap();
    let mut persistence = DocumentPersistenceStore::default();
    persistence
        .register(document, opened.grant.id, opened.observed, &opened.text)
        .unwrap();
    let pending = persistence.begin(&snapshot).unwrap();

    let result = grants.execute_save(pending.grant, &pending.plan);
    let reduction = persistence
        .complete(document, pending.plan.generation, result)
        .unwrap();
    assert!(matches!(
        reduction,
        SaveReduction::Durable(checkpoint) if checkpoint.document == document
    ));
}

#[test]
fn temporary_name_collision_retries_without_truncating_the_collision() {
    let path = unique_file("temp-collision", b"old");
    let contents = read_atomic_file(&path, 1024, false).unwrap();
    let seed = 0x5a17;
    let collision = path.parent().unwrap().join(format!(
        ".{}.aterm-{}-{seed}-0.tmp",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&collision, "do not touch").unwrap();

    assert!(matches!(
        commit_atomic_bytes_with_seed(
            &contents.baseline,
            b"new",
            seed,
            Flush::Durable,
            Directories::Create
        ),
        AtomicCommitResult::Committed(_)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert_eq!(fs::read_to_string(&collision).unwrap(), "do not touch");
}

#[cfg(unix)]
#[test]
fn config_symlink_chain_is_bound_and_preserved_while_ordinary_grants_fail_closed() {
    use std::os::unix::fs::symlink;

    let scratch = crate::test_scratch_dir("retarget");
    let dir = scratch.path();
    let first = dir.join("first.toml");
    let real_parent = dir.join("real");
    fs::create_dir_all(&real_parent).unwrap();
    let second = real_parent.join("second.toml");
    let logical = dir.join("aterm.toml");
    let parent_alias = dir.join("parent-alias");
    let chained_final = real_parent.join("chained.toml");
    let chained_logical = parent_alias.join("chained.toml");
    fs::write(&first, "theme = \"Default\"\n").unwrap();
    fs::write(&second, "theme = \"Nord\"\n").unwrap();
    owner_only(&first);
    owner_only(&second);
    symlink("first.toml", &logical).unwrap();
    symlink(&real_parent, &parent_alias).unwrap();
    symlink("../first.toml", &chained_final).unwrap();

    let mut strict_grants = DocumentGrantStore::new();
    assert!(matches!(
        strict_grants.open_local(&file_uri(&logical), GrantAccess::ReadWrite, 4096),
        Err(DocumentHostError::SymlinkComponent { .. })
    ));
    let mut grants = DocumentGrantStore::new();
    let opened = grants
        .open_local_config(&file_uri(&logical), GrantAccess::ReadWrite, 4096)
        .unwrap();
    assert_eq!(opened.text, "theme = \"Default\"\n");
    assert_eq!(opened.grant.logical_path(), logical);
    assert_eq!(
        opened.grant.target().target_path(),
        first.canonicalize().unwrap()
    );
    assert!(matches!(
        grants.open_local(
            &file_uri(&parent_alias.join("second.toml")),
            GrantAccess::ReadWrite,
            4096,
        ),
        Err(DocumentHostError::SymlinkComponent { .. })
    ));
    let opened_parent = grants
        .open_local_config(
            &file_uri(&parent_alias.join("second.toml")),
            GrantAccess::ReadWrite,
            4096,
        )
        .unwrap();
    assert_eq!(opened_parent.text, "theme = \"Nord\"\n");
    assert_eq!(
        opened_parent.grant.logical_path(),
        parent_alias.join("second.toml")
    );
    assert_eq!(
        opened_parent.grant.target().target_path(),
        second.canonicalize().unwrap()
    );
    let opened_chain = grants
        .open_local_config(&file_uri(&chained_logical), GrantAccess::ReadWrite, 4096)
        .unwrap();
    assert_eq!(opened_chain.grant.id, opened.grant.id);
    assert_eq!(opened_chain.grant.logical_path(), chained_logical);
    assert_eq!(
        opened_chain.grant.target().target_path(),
        first.canonicalize().unwrap()
    );

    let mut documents = crate::test_document_store();
    let document = documents.open(opened.grant.canonical_uri.clone(), "updated".to_string());
    let mut reducer = SaveReducer::new(document, opened.observed);
    let plan = reducer
        .begin(&documents.snapshot(document).unwrap())
        .unwrap();
    assert!(matches!(
        grants.execute_save(opened.grant.id, &plan),
        AtomicSaveResult::Committed(_)
    ));
    assert_eq!(fs::read_to_string(&first).unwrap(), "updated");
    assert_eq!(fs::read_to_string(&second).unwrap(), "theme = \"Nord\"\n");
    assert!(
        fs::symlink_metadata(&logical)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(&logical).unwrap(),
        PathBuf::from("first.toml")
    );
    assert!(
        fs::symlink_metadata(&chained_final)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(&chained_final).unwrap(),
        PathBuf::from("../first.toml")
    );

    let parent_document = documents.open(
        opened_parent.grant.canonical_uri.clone(),
        "parent-updated".to_string(),
    );
    let mut parent_reducer = SaveReducer::new(parent_document, opened_parent.observed);
    let parent_plan = parent_reducer
        .begin(&documents.snapshot(parent_document).unwrap())
        .unwrap();
    assert!(matches!(
        grants.execute_save(opened_parent.grant.id, &parent_plan),
        AtomicSaveResult::Committed(_)
    ));
    assert_eq!(fs::read_to_string(&second).unwrap(), "parent-updated");
    assert!(
        fs::symlink_metadata(&parent_alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&parent_alias).unwrap(), real_parent);
}

#[cfg(unix)]
#[test]
fn final_symlink_swap_is_a_conflict_and_never_redirects_the_save() {
    use std::os::unix::fs::symlink;

    let scratch = crate::test_scratch_dir("link-swap");
    let dir = scratch.path();
    let first = dir.join("first.toml");
    let second = dir.join("second.toml");
    let logical = dir.join("aterm.toml");
    fs::write(&first, "theme = \"Default\"\n").unwrap();
    fs::write(&second, "theme = \"Dracula\"\n").unwrap();
    owner_only(&first);
    owner_only(&second);
    symlink("first.toml", &logical).unwrap();

    let mut grants = DocumentGrantStore::new();
    let direct = grants
        .open_local(&file_uri(&first), GrantAccess::ReadWrite, 4096)
        .unwrap();
    let opened = grants
        .open_local_config(&file_uri(&logical), GrantAccess::ReadWrite, 4096)
        .unwrap();
    assert_eq!(opened.grant.id, direct.grant.id);
    assert_eq!(opened.grant.logical_path(), logical);
    let mut documents = crate::test_document_store();
    let document = documents.open(
        opened.grant.canonical_uri.clone(),
        "theme = \"Nord\"\n".to_string(),
    );
    let mut reducer = SaveReducer::new(document, opened.observed);
    let plan = reducer
        .begin(&documents.snapshot(document).unwrap())
        .unwrap();

    fs::remove_file(&logical).unwrap();
    symlink("second.toml", &logical).unwrap();
    assert!(matches!(
        grants.execute_save(opened.grant.id, &plan),
        AtomicSaveResult::Conflict { .. }
    ));
    assert_eq!(fs::read_to_string(&first).unwrap(), "theme = \"Default\"\n");
    assert_eq!(
        fs::read_to_string(&second).unwrap(),
        "theme = \"Dracula\"\n"
    );
    assert_eq!(
        fs::read_link(&logical).unwrap(),
        PathBuf::from("second.toml")
    );
}

#[cfg(unix)]
#[test]
fn recreated_parent_symlink_to_same_destination_is_still_a_conflict() {
    use std::os::unix::fs::symlink;

    let scratch = crate::test_scratch_dir("parent-relink");
    let dir = scratch.path();
    let real = dir.join("real");
    let alias = dir.join("config");
    let displaced_alias = dir.join("config.previous");
    fs::create_dir_all(&real).unwrap();
    let target = real.join("aterm.toml");
    let logical = alias.join("aterm.toml");
    fs::write(&target, "theme = \"Default\"\n").unwrap();
    owner_only(&target);
    symlink(&real, &alias).unwrap();

    let mut grants = DocumentGrantStore::new();
    let opened = grants
        .open_local_config(&file_uri(&logical), GrantAccess::ReadWrite, 4096)
        .unwrap();
    let mut documents = crate::test_document_store();
    let document = documents.open(
        opened.grant.canonical_uri.clone(),
        "theme = \"Nord\"\n".to_string(),
    );
    let mut reducer = SaveReducer::new(document, opened.observed);
    let plan = reducer
        .begin(&documents.snapshot(document).unwrap())
        .unwrap();

    fs::rename(&alias, &displaced_alias).unwrap();
    symlink(&real, &alias).unwrap();
    assert!(matches!(
        grants.execute_save(opened.grant.id, &plan),
        AtomicSaveResult::Conflict { .. }
    ));
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "theme = \"Default\"\n"
    );
    assert_eq!(fs::read_link(&alias).unwrap(), real);
}

/// The names in `folder`, sorted.
#[cfg(unix)]
fn names_in(folder: &Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Put a link to the folder `to` where the folder `at` is, `at` moved to `aside`: what a
/// process that may write `at`'s parent, but not `to`, can do between two steps of a commit.
#[cfg(unix)]
fn link_in(at: &Path, aside: &Path, to: &Path) {
    fs::rename(at, aside).unwrap();
    std::os::unix::fs::symlink(to, at).unwrap();
}

/// Undo [`link_in`]: the folder at `aside` back at `at`, in place of the link.
#[cfg(unix)]
fn folder_back(at: &Path, aside: &Path) {
    fs::remove_file(at).unwrap();
    fs::rename(aside, at).unwrap();
}

/// A commit's steps as a hook saw them, and the hook that records them before it runs
/// `act` at each.
#[cfg(unix)]
fn recording(
    mut act: impl FnMut(seam::Step) + 'static,
) -> (
    std::rc::Rc<std::cell::RefCell<Vec<seam::Step>>>,
    impl FnMut(seam::Step) + 'static,
) {
    let steps = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&steps);
    (steps, move |step| {
        seen.borrow_mut().push(step);
        act(step);
    })
}

/// While the file a document names is missing, a process that swaps the document's folder
/// for a link to a folder it may not write, between a commit's steps (the link in while the
/// temporary is made, the folder back for the last check, the link in again for the
/// publish), never makes the file, or anything else, in that folder: the folder the missing
/// aterm.toml in force would be made in, say, where a save checked as some other file's
/// would otherwise make it. Every step works in the folder the commit opened, so the bytes
/// land in the document's own folder, wherever it was moved, and the save reports the
/// binding it no longer holds.
#[cfg(unix)]
#[test]
fn a_folder_swapped_for_a_link_mid_commit_never_makes_the_missing_file_elsewhere() {
    use seam::Step;

    let dir = crate::test_scratch_dir("swap-mid-commit");
    let root = fs::canonicalize(dir.path()).unwrap();
    let scratch = root.join("scratch");
    let aside = root.join("scratch.aside");
    let kept = root.join("kept");
    fs::create_dir(&scratch).unwrap();
    fs::create_dir(&kept).unwrap();
    let path = scratch.join("aterm.toml");
    let text = "[harness]\noccupancy = \"cruise\"\n";
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text(text);

    let (at, away, to) = (scratch.clone(), aside.clone(), kept.clone());
    let (steps, hook) = recording(move |step| match step {
        Step::Folder | Step::Opened | Step::Lock | Step::Observe => {}
        Step::Temporary | Step::Publish => link_in(&at, &away, &to),
        Step::Validate => folder_back(&at, &away),
    });
    let reduction = seam::with(hook, || opened.save());

    assert_eq!(
        *steps.borrow(),
        [
            Step::Folder,
            Step::Opened,
            Step::Lock,
            Step::Temporary,
            Step::Validate,
            Step::Publish,
            Step::Observe
        ]
    );
    assert_eq!(
        names_in(&kept),
        Vec::<std::ffi::OsString>::new(),
        "a commit put an entry in the folder its document's folder was swapped for a link to"
    );
    assert!(
        matches!(reduction, SaveReduction::ReconcileRequired { .. }),
        "{reduction:?}"
    );
    assert_eq!(fs::read_to_string(aside.join("aterm.toml")).unwrap(), text);
}

/// NEGATIVE CONTROL for [`a_folder_swapped_for_a_link_mid_commit_never_makes_the_missing_file_elsewhere`]:
/// the same save with nothing swapped takes the same steps and makes the file in the
/// document's folder.
#[cfg(unix)]
#[test]
fn the_same_commit_with_no_swap_makes_the_missing_file_in_its_folder() {
    use seam::Step;

    let dir = crate::test_scratch_dir("no-swap");
    let root = fs::canonicalize(dir.path()).unwrap();
    let scratch = root.join("scratch");
    let kept = root.join("kept");
    fs::create_dir(&scratch).unwrap();
    fs::create_dir(&kept).unwrap();
    let path = scratch.join("aterm.toml");
    let text = "[harness]\noccupancy = \"cruise\"\n";
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text(text);

    let (steps, hook) = recording(|_| {});
    let reduction = seam::with(hook, || opened.save());

    assert_eq!(
        *steps.borrow(),
        [
            Step::Folder,
            Step::Opened,
            Step::Lock,
            Step::Temporary,
            Step::Validate,
            Step::Publish,
            Step::Observe
        ]
    );
    assert!(
        matches!(reduction, SaveReduction::Durable(_)),
        "{reduction:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    assert_eq!(names_in(&kept), Vec::<std::ffi::OsString>::new());
}

/// A link put in place of a folder above the document's folder just before a commit opens
/// its folder, and the folder put back just after (before any check by path can see the
/// link), is refused there: the commit opens its folder without following a link at any
/// level, so it never works in the folder such a link names, whose own name matches
/// (`aterm`), not even for its save lock.
#[cfg(unix)]
#[test]
fn a_link_on_the_path_only_while_the_commit_opens_its_folder_is_refused() {
    use seam::Step;

    let dir = crate::test_scratch_dir("link-on-open");
    let root = fs::canonicalize(dir.path()).unwrap();
    let agent = root.join("agent");
    let aside = root.join("agent.aside");
    let home = root.join("home");
    let kept = home.join("aterm");
    fs::create_dir_all(agent.join("aterm")).unwrap();
    fs::create_dir_all(&kept).unwrap();
    let path = agent.join("aterm").join("aterm.toml");
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text("[harness]\noccupancy = \"cruise\"\n");

    let (at, away, to) = (agent.clone(), aside.clone(), home.clone());
    let (steps, hook) = recording(move |step| match step {
        Step::Folder => link_in(&at, &away, &to),
        Step::Opened => folder_back(&at, &away),
        Step::Lock | Step::Temporary | Step::Validate | Step::Publish | Step::Observe => {}
    });
    let reduction = seam::with(hook, || opened.save());

    assert_eq!(*steps.borrow(), [Step::Folder]);
    assert_eq!(
        names_in(&kept),
        Vec::<std::ffi::OsString>::new(),
        "a commit put an entry in the folder a link above its document's folder named"
    );
    assert!(
        matches!(reduction, SaveReduction::Conflict(_)),
        "{reduction:?}"
    );
    assert!(!aside.join("aterm").join("aterm.toml").exists());
}

/// While the file a document names is missing, its folder swapped for a link to another
/// folder just before any one step of a commit, and the link left there, never gets anything
/// in the folder it names, and the save is never reported durable: before the folder is
/// opened the link refuses it, after it each step works in the folder opened (the bytes land
/// there when the link came only for the publish), and a check by path sees the link.
#[cfg(unix)]
#[test]
fn a_link_put_in_place_of_the_folder_before_any_step_gets_nothing_in_the_folder_it_names() {
    use seam::Step;

    for at_step in [
        Step::Folder,
        Step::Opened,
        Step::Lock,
        Step::Temporary,
        Step::Validate,
        Step::Publish,
        Step::Observe,
    ] {
        let dir = crate::test_scratch_dir("link-each-step");
        let root = fs::canonicalize(dir.path()).unwrap();
        let scratch = root.join("scratch");
        let aside = root.join("scratch.aside");
        let kept = root.join("kept");
        fs::create_dir(&scratch).unwrap();
        fs::create_dir(&kept).unwrap();
        let path = scratch.join("aterm.toml");
        let mut opened = open_document(&path, ALLOW_MISSING);
        opened.set_text("[harness]\noccupancy = \"cruise\"\n");

        let (at, away, to) = (scratch.clone(), aside.clone(), kept.clone());
        let (_, hook) = recording(move |step| {
            if step == at_step {
                link_in(&at, &away, &to);
            }
        });
        let reduction = seam::with(hook, || opened.save());

        assert_eq!(
            names_in(&kept),
            Vec::<std::ffi::OsString>::new(),
            "{at_step:?}: a commit put an entry in the folder a link in its folder's place named"
        );
        assert!(
            !matches!(reduction, SaveReduction::Durable(_)),
            "{at_step:?}: {reduction:?}"
        );
    }
}

/// A document's folder moved away, and a real folder made in its place, between a commit's
/// steps (after its temporary, before its last check) fails the save before anything is
/// published, as a folder that went missing while saving: the path no longer reaches the
/// folder the commit holds (device and inode), so neither that folder, wherever it went, nor
/// the new one gets the file, and the temporary goes with the failure.
#[cfg(unix)]
#[test]
fn a_folder_replaced_mid_commit_is_refused_before_anything_is_published() {
    use seam::Step;

    let dir = crate::test_scratch_dir("replaced-mid-commit");
    let root = fs::canonicalize(dir.path()).unwrap();
    let scratch = root.join("scratch");
    let aside = root.join("scratch.aside");
    fs::create_dir(&scratch).unwrap();
    let path = scratch.join("aterm.toml");
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text("[harness]\noccupancy = \"cruise\"\n");

    let (at, away) = (scratch.clone(), aside.clone());
    let (steps, hook) = recording(move |step| {
        if step == Step::Validate {
            fs::rename(&at, &away).unwrap();
            fs::create_dir(&at).unwrap();
        }
    });
    let reduction = seam::with(hook, || opened.save());

    assert_eq!(
        *steps.borrow(),
        [
            Step::Folder,
            Step::Opened,
            Step::Lock,
            Step::Temporary,
            Step::Validate
        ]
    );
    assert!(
        matches!(
            reduction,
            SaveReduction::Failed {
                stage: AtomicSaveStage::RenameTarget,
                cause: Some(SaveCause {
                    kind: std::io::ErrorKind::NotFound,
                    touch: SaveTouch::Folder,
                }),
                ..
            }
        ),
        "{reduction:?}"
    );
    assert_eq!(names_in(&scratch), Vec::<std::ffi::OsString>::new());
    assert_eq!(
        names_in(&aside),
        [std::ffi::OsString::from(".aterm.toml.aterm-write.lock")],
        "only the save lock stays in the folder the commit held"
    );
}

/// A document's folder moved away, and a real folder made in its place, once the file is
/// published in it (before the commit's last check) is never reported durable: the file is
/// in the folder the commit held, wherever it went, and the path names another folder now,
/// so the save asks to be reconciled.
#[cfg(unix)]
#[test]
fn a_folder_replaced_once_the_file_is_published_is_never_reported_durable() {
    use seam::Step;

    let dir = crate::test_scratch_dir("replaced-published");
    let root = fs::canonicalize(dir.path()).unwrap();
    let scratch = root.join("scratch");
    let aside = root.join("scratch.aside");
    fs::create_dir(&scratch).unwrap();
    let path = scratch.join("aterm.toml");
    let text = "[harness]\noccupancy = \"cruise\"\n";
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text(text);

    let (at, away) = (scratch.clone(), aside.clone());
    let (steps, hook) = recording(move |step| {
        if step == Step::Observe {
            fs::rename(&at, &away).unwrap();
            fs::create_dir(&at).unwrap();
        }
    });
    let reduction = seam::with(hook, || opened.save());

    assert_eq!(
        *steps.borrow(),
        [
            Step::Folder,
            Step::Opened,
            Step::Lock,
            Step::Temporary,
            Step::Validate,
            Step::Publish,
            Step::Observe
        ]
    );
    assert!(
        matches!(reduction, SaveReduction::ReconcileRequired { .. }),
        "{reduction:?}"
    );
    assert_eq!(fs::read_to_string(aside.join("aterm.toml")).unwrap(), text);
    assert_eq!(names_in(&scratch), Vec::<std::ffi::OsString>::new());
}

/// A document's folder moved away, and a real folder made in its place, just before the
/// commit takes its save lock leaves that lock guarding no save of the path: the commit's
/// check after the lock sees the path reach another folder, and the commit starts again in
/// the folder the path names now, where the save lands. It starts again however long that
/// first attempt took: here past the whole retry budget, as on a loaded host.
#[cfg(unix)]
#[test]
fn a_folder_replaced_before_the_lock_starts_the_commit_again_in_the_new_folder() {
    use seam::Step;

    let dir = crate::test_scratch_dir("replaced-before-lock");
    let root = fs::canonicalize(dir.path()).unwrap();
    let scratch = root.join("scratch");
    let aside = root.join("scratch.aside");
    fs::create_dir(&scratch).unwrap();
    let path = scratch.join("aterm.toml");
    let text = "[harness]\noccupancy = \"cruise\"\n";
    let mut opened = open_document(&path, ALLOW_MISSING);
    opened.set_text(text);

    let (at, away) = (scratch.clone(), aside.clone());
    let mut replaced = false;
    let (steps, hook) = recording(move |step| {
        if step == Step::Lock && !replaced {
            replaced = true;
            fs::rename(&at, &away).unwrap();
            fs::create_dir(&at).unwrap();
            std::thread::sleep(PREFLIGHT_RETRY_BUDGET + std::time::Duration::from_millis(1));
        }
    });
    let reduction = seam::with(hook, || opened.save());

    assert_eq!(
        *steps.borrow(),
        [
            Step::Folder,
            Step::Opened,
            Step::Lock,
            Step::Folder,
            Step::Opened,
            Step::Lock,
            Step::Temporary,
            Step::Validate,
            Step::Publish,
            Step::Observe
        ]
    );
    assert!(
        matches!(reduction, SaveReduction::Durable(_)),
        "{reduction:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    assert_eq!(
        names_in(&aside),
        [std::ffi::OsString::from(".aterm.toml.aterm-write.lock")],
        "only the first attempt's save lock stays in the folder moved away"
    );
}

/// A commit's folder is reached by its path alone, never through a link: the path it was
/// opened by reaches it, and so does the path it was moved to; nothing there, or another
/// folder, does not; and a link in its place is refused as a link even when the link names
/// the folder itself.
#[cfg(unix)]
#[test]
fn a_held_folder_is_never_reached_through_a_link_even_to_itself() {
    let dir = crate::test_scratch_dir("held-folder");
    let root = fs::canonicalize(dir.path()).unwrap();
    let scratch = root.join("scratch");
    let aside = root.join("scratch.aside");
    fs::create_dir(&scratch).unwrap();
    let held = Folder::open(&scratch, false).unwrap();
    assert!(held.is_reached_by(&scratch).unwrap(), "its own path");

    fs::rename(&scratch, &aside).unwrap();
    assert!(
        !held.is_reached_by(&scratch).unwrap(),
        "nothing at its path"
    );
    assert!(held.is_reached_by(&aside).unwrap(), "the path it moved to");

    std::os::unix::fs::symlink(&aside, &scratch).unwrap();
    let through_link = held.is_reached_by(&scratch);
    assert!(
        through_link.as_ref().is_err_and(folder::is_link),
        "a link to it: {through_link:?}"
    );

    fs::remove_file(&scratch).unwrap();
    fs::create_dir(&scratch).unwrap();
    assert!(!held.is_reached_by(&scratch).unwrap(), "another folder");
}

/// A config commit (`Settings`, which makes the folders a missing aterm.toml belongs in) whose
/// path has a folder swapped for a link just before it reaches into it, and back just after,
/// makes nothing where the link points, not even a folder: the missing folders are made one
/// by one, each in the folder above it as opened with no link on its path.
#[cfg(unix)]
#[test]
fn a_config_commit_never_makes_its_missing_folders_where_a_link_points() {
    use seam::Step;

    let dir = crate::test_scratch_dir("config-swap");
    let root = fs::canonicalize(dir.path()).unwrap();
    let agent = root.join("agent");
    let aside = root.join("agent.aside");
    let home = root.join("home");
    fs::create_dir(&agent).unwrap();
    fs::create_dir(&home).unwrap();
    let path = agent.join("aterm").join("aterm.toml");
    let missing = read_config_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, true).unwrap();

    let (at, away, to) = (agent.clone(), aside.clone(), home.clone());
    let (steps, hook) = recording(move |step| match step {
        Step::Folder => link_in(&at, &away, &to),
        Step::Opened => folder_back(&at, &away),
        Step::Lock | Step::Temporary | Step::Validate | Step::Publish | Step::Observe => {}
    });
    let result = seam::with(hook, || {
        commit_atomic_bytes(&missing.baseline, b"[harness]\noccupancy = \"cruise\"\n")
    });

    assert_eq!(*steps.borrow(), [Step::Folder]);
    assert_eq!(
        names_in(&home),
        Vec::<std::ffi::OsString>::new(),
        "a config commit made an entry where a link on its path pointed"
    );
    assert!(
        matches!(result, AtomicCommitResult::Conflict { .. }),
        "{result:?}"
    );
    assert_eq!(names_in(&aside), Vec::<std::ffi::OsString>::new());
}

/// NEGATIVE CONTROL for [`a_config_commit_never_makes_its_missing_folders_where_a_link_points`]:
/// the same commit with nothing swapped makes the folder and the file where its path names.
#[cfg(unix)]
#[test]
fn the_same_config_commit_with_no_swap_makes_its_folder_and_file() {
    use seam::Step;

    let dir = crate::test_scratch_dir("config-no-swap");
    let root = fs::canonicalize(dir.path()).unwrap();
    let agent = root.join("agent");
    fs::create_dir(&agent).unwrap();
    let path = agent.join("aterm").join("aterm.toml");
    let missing = read_config_atomic_file(&path, DEFAULT_DOCUMENT_LIMIT, true).unwrap();

    let (steps, hook) = recording(|_| {});
    let result = seam::with(hook, || {
        commit_atomic_bytes(&missing.baseline, b"font_px = 14\n")
    });

    assert_eq!(
        *steps.borrow(),
        [
            Step::Folder,
            Step::Opened,
            Step::Lock,
            Step::Temporary,
            Step::Validate,
            Step::Publish,
            Step::Observe
        ]
    );
    assert!(
        matches!(result, AtomicCommitResult::Committed(_)),
        "{result:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "font_px = 14\n");
}

/// The two openers of a commit's folder, the one-call open (macOS's `O_NOFOLLOW_ANY`) and
/// the name-by-name walk (Linux's), answer alike: the folder where the path reaches it by
/// real folders alone; a link in the place of the folder or of any folder above it is
/// refused as a link; a file where a folder of the path would be, or nothing there, as
/// missing.
#[cfg(unix)]
#[test]
fn both_folder_openers_refuse_a_link_anywhere_on_the_path() {
    let dir = crate::test_scratch_dir("openers");
    let root = fs::canonicalize(dir.path()).unwrap();
    fs::create_dir_all(root.join("real").join("inner")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    fs::write(root.join("file"), "").unwrap();

    type Answer = fn(&std::io::Error) -> bool;
    let cases: [(PathBuf, Option<Answer>); 6] = [
        (root.join("real").join("inner"), None),
        (root.clone(), None),
        (root.join("link"), Some(folder::is_link)),
        (root.join("link").join("inner"), Some(folder::is_link)),
        (root.join("file"), Some(folder::is_missing)),
        (root.join("file").join("inner"), Some(folder::is_missing)),
    ];
    for (path, refused) in cases {
        let walked = folder::walk(&path).map(|_| ());
        let opened = Folder::open(&path, false).map(|_| ());
        for (opener, answer) in [("walk", walked), ("open", opened)] {
            match (refused, answer) {
                (None, Ok(())) => {}
                (Some(expected), Err(error)) if expected(&error) => {}
                (_, answer) => panic!("{opener} {}: {answer:?}", path.display()),
            }
        }
    }
    assert!(
        folder::is_missing(&Folder::open(&root.join("nothing"), false).err().unwrap()),
        "nothing there is missing"
    );
}

/// A document under a folder this user may only search (`--x`, no read) saves, as it did when
/// a commit worked by path: opening the commit's folder with no link on its path needs only
/// the permissions a path's lookup needs (macOS's one open, Linux's walk by `O_PATH`).
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn a_document_under_a_folder_this_user_may_only_search_saves() {
    use std::os::unix::fs::PermissionsExt as _;

    /// Gives `above` its read bit back however the test ends, so its scratch can go.
    struct Readable(PathBuf);
    impl Drop for Readable {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }

    let dir = crate::test_scratch_dir("search-only-above");
    let root = fs::canonicalize(dir.path()).unwrap();
    let above = root.join("above");
    let docs = above.join("docs");
    fs::create_dir_all(&docs).unwrap();
    let path = docs.join("note.md");
    fs::write(&path, "before\n").unwrap();
    let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
    opened.set_text("after\n");

    let _readable = Readable(above.clone());
    fs::set_permissions(&above, fs::Permissions::from_mode(0o100)).unwrap();
    assert_eq!(
        fs::read_dir(&above).err().map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied),
        "the folder above is search-only for this user"
    );
    let reduction = opened.save();
    assert!(
        matches!(reduction, SaveReduction::Durable(_)),
        "{reduction:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
}

/// A document in a folder this user may write and search but not list (`-wx`, a drop box)
/// saves, as it did when a commit worked by path: the commit holds its folder to look names
/// up in alone (macOS's `O_SEARCH`, Linux's `O_PATH`), never for reading. macOS syncs the
/// folder through that handle, so the save is durable; Linux opens the folder for reading
/// to sync it, which it may not, so there the bytes are published and the save says it
/// could not sync the folder.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn a_document_in_a_folder_this_user_may_write_and_search_but_not_list_saves() {
    use std::os::unix::fs::PermissionsExt as _;

    /// Gives `docs` its read bit back however the test ends, so its scratch can go.
    struct Listable(PathBuf);
    impl Drop for Listable {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }

    let dir = crate::test_scratch_dir("write-search-only");
    let root = fs::canonicalize(dir.path()).unwrap();
    let docs = root.join("docs");
    fs::create_dir(&docs).unwrap();
    let path = docs.join("note.md");
    fs::write(&path, "before\n").unwrap();
    let mut opened = open_document(&path, AdmitOptions::DOCUMENT);
    opened.set_text("after\n");

    let _listable = Listable(docs.clone());
    fs::set_permissions(&docs, fs::Permissions::from_mode(0o300)).unwrap();
    assert_eq!(
        fs::read_dir(&docs).err().map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied),
        "the document's folder is write and search only for this user"
    );
    let reduction = opened.save();
    #[cfg(target_vendor = "apple")]
    assert!(
        matches!(reduction, SaveReduction::Durable(_)),
        "{reduction:?}"
    );
    #[cfg(not(target_vendor = "apple"))]
    assert!(
        matches!(
            reduction,
            SaveReduction::ReconcileRequired {
                stage: AtomicSaveStage::SyncDirectory,
                ..
            }
        ),
        "{reduction:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
}

/// NEGATIVE CONTROL: a save lock owned by another UID must be refused with
/// [`SaveTouch::Lock`] and [`std::io::ErrorKind::PermissionDenied`] before `fchmod` or
/// locking, even when the caller has privilege to open it.
#[cfg(unix)]
#[test]
fn write_lock_owner_check_rejects_mismatched_uid_with_lock_touch() {
    let lock_path = Path::new("/tmp/.aterm.toml.aterm-write.lock");
    assert!(ensure_write_lock_owner(lock_path, 501, 501).is_ok());
    let failure = ensure_write_lock_owner(lock_path, 502, 501).unwrap_err();
    assert_eq!(
        failure.cause,
        Some(SaveCause {
            kind: std::io::ErrorKind::PermissionDenied,
            touch: SaveTouch::Lock,
        })
    );
    assert!(failure.message.contains("not owned by this user"));
}
