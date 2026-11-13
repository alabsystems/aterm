// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn doc_lane_refuses_before_asking() {
    let asked = std::cell::RefCell::new(Vec::new());
    let mut ask = |query: DocQuery| {
        asked.borrow_mut().push(query);
        Ok(DocAnswer::Unknown)
    };
    for (rest, answer) in [
        (
            "",
            format!("ERR usage: {}\n", amacs::protocol::doc::DOC_USAGE),
        ),
        (
            "frob",
            format!("ERR usage: {}\n", amacs::protocol::doc::DOC_USAGE),
        ),
        ("ls uri=/etc/hosts", "ERR bad uri=/etc/hosts\n".to_string()),
        ("ls all", "ERR unknown option all\n".to_string()),
        (
            "ls uri=file://host/x",
            "ERR bad uri=file://host/x (remote file URI authorities are not allowed)\n".to_string(),
        ),
        ("read 0", "ERR bad doc=0\n".to_string()),
        (
            "read 3 lines=4-2",
            "ERR bad lines=4-2 (want <first>-<last>, <first>- or <line>, from 1)\n".to_string(),
        ),
        (
            "log 3",
            "ERR unsupported doc log: not served yet\n".to_string(),
        ),
    ] {
        assert_eq!(serve_doc(rest, &mut ask), answer, "doc {rest}");
    }
    assert_eq!(*asked.borrow(), Vec::new());
    assert_eq!(serve_doc("read 7", &mut ask), "ERR unknown doc 7\n");
    assert_eq!(*asked.borrow(), vec![DocQuery::Read(DocNames::Id(7))]);
}

#[test]
fn doc_uri_spellings_reach_the_store_key() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-doc-spell-")
        .tempdir()
        .unwrap();
    let path = dir.path().join("a b.txt");
    std::fs::write(&path, b"x\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let real =
        crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&path).unwrap())
            .unwrap();
    let names = spellings(&uri).unwrap();
    assert_eq!(names.first(), Some(&uri));
    assert!(names.contains(&real), "{names:?}");
    let gone = crate::native_document_host::path_to_file_uri(&dir.path().join("gone.txt")).unwrap();
    let real_dir = std::fs::canonicalize(dir.path()).unwrap();
    let real_gone =
        crate::native_document_host::path_to_file_uri(&real_dir.join("gone.txt")).unwrap();
    let names = spellings(&gone).unwrap();
    assert_eq!(names.first(), Some(&gone));
    assert!(names.contains(&real_gone), "{names:?}");
    let deep = crate::native_document_host::path_to_file_uri(
        &dir.path().join("no-such-dir").join("x.txt"),
    )
    .unwrap();
    let real_deep =
        crate::native_document_host::path_to_file_uri(&real_dir.join("no-such-dir").join("x.txt"))
            .unwrap();
    assert!(spellings(&deep).unwrap().contains(&real_deep));
}
