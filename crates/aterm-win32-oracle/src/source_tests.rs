// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn pinned_source_shapes_keep_gates_aliases_and_nested_types() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("mod.rs");
    std::fs::write(&file, r#"
#[cfg(feature = "Win32_Foundation")]
windows_link::link!("test.dll" "system" fn Query(value : *const ITEM, done : Option<unsafe extern "system" fn(*mut core::ffi::c_void, u32)>) -> i32);
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ITEM {
    pub callback: Option<unsafe extern "system" fn(u32, *mut u8)>,
    pub bytes: [u8; 4],
}
impl Default for ITEM {
    fn default() -> Self { unsafe { core::mem::zeroed() } }
}
pub use Query as QueryAlias;
#[macro_export]
macro_rules! literal { ($s:literal) => { $s }; }
pub use literal;
"#).unwrap();
    let items = parse_file(&file, &["Win32".into()]).unwrap();
    assert_eq!(items.len(), 4);
    assert!(items[0].features.contains("Win32_Foundation"));
    let query = function(&items[0]).unwrap();
    assert_eq!(query.abi, "system");
    assert_eq!(query.params.len(), 2);
    assert!(query.params[1].1.ends_with("c_void, u32)>"));
    assert_eq!(query.ret, "i32");
    assert_eq!(fields(&items[1]).unwrap().len(), 2);
    assert_eq!(items[1].impls.len(), 1);
    assert_eq!(
        (items[2].kind.as_str(), items[2].name.as_str()),
        ("use", "QueryAlias")
    );
    assert_eq!(items[3].kind, "macro");
    std::fs::write(&file, "pub enum Unknown { A }\n").unwrap();
    assert!(
        parse_file(&file, &[]).is_err(),
        "unknown declaration forms refuse generation"
    );
}
