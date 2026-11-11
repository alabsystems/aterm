// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn plist(text: &str) -> Option<Print> {
    Some(text.as_bytes().to_vec())
}

#[test]
fn a_change_counts_only_when_it_reads_cleanly_twice() {
    let mut base = plist("1");
    assert!(!replaced(&mut base, plist("1"), || panic!(
        "not read again"
    )));
    assert!(!replaced(&mut base, None, || panic!("not read again")));
    assert_eq!(base, plist("1"));
    assert!(!replaced(&mut base, plist("2"), || plist("3")));
    assert!(!replaced(&mut base, plist("2"), || None));
    assert!(replaced(&mut base, plist("2"), || plist("2")));
    assert_eq!(base, plist("1"));
}

#[test]
fn with_no_baseline_the_first_clean_read_is_it() {
    let mut base = None;
    assert!(!replaced(&mut base, None, || panic!("not read again")));
    assert_eq!(base, None);
    assert!(!replaced(&mut base, plist("1"), || panic!(
        "not read again"
    )));
    assert_eq!(base, plist("1"));
    assert!(replaced(&mut base, plist("2"), || plist("2")));
}

#[test]
fn a_bundles_new_info_plist_is_a_new_build() {
    let dir = crate::test_scratch_dir("watch");
    let app = dir.path().join("X.app");
    let exe = app.join("Contents").join("MacOS").join("aterm");
    std::fs::create_dir_all(exe.parent().expect("parent")).expect("mkdir");
    std::fs::write(&exe, b"exe").expect("exe");
    let mut watch = Watch::start(exe.clone(), &exe);
    assert_eq!(watch.baseline, None, "no Info.plist yet");
    let info = app.join("Contents").join("Info.plist");
    std::fs::write(&info, b"<plist>1</plist>").expect("plist");
    assert!(!watch.replaced(), "the first clean read is the baseline");
    assert!(!watch.replaced());
    std::fs::write(&info, b"<plist>2</plist>").expect("plist");
    assert!(watch.replaced());
}

#[test]
fn an_update_during_the_copys_first_exec_is_a_replaced_origin() {
    let dir = crate::test_scratch_dir("watch");
    let bundle = |name: &str, plist: &[u8]| {
        let exe = dir.path().join(name).join("Contents/MacOS/aterm");
        std::fs::create_dir_all(exe.parent().expect("parent")).expect("mkdir");
        std::fs::write(&exe, b"exe").expect("exe");
        std::fs::write(dir.path().join(name).join("Contents/Info.plist"), plist).expect("plist");
        exe
    };
    let copy = bundle("copy/X.app", b"<plist>1</plist>");
    let origin = bundle("X.app", b"<plist>2</plist>");
    assert!(Watch::start(origin.clone(), &copy).replaced());
    assert!(
        !Watch::start(origin.clone(), &origin).replaced(),
        "the control"
    );
}
