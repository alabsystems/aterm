// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_path_is_spelt_as_pathlib_spells_it() {
    let spelt = |text: &str| PyPath::new(OsStr::new(text)).to_string();
    assert_eq!(spelt("/a//b/./c/"), "/a/b/c");
    assert_eq!(spelt("//a/b"), "//a/b");
    assert_eq!(spelt("///a"), "/a");
    assert_eq!(spelt("a/../b"), "a/../b");
    assert_eq!(spelt(""), ".");
    #[cfg(unix)]
    {
        let lineage: Vec<String> = PyPath::new(OsStr::new("/a/b"))
            .lineage()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(lineage, ["/", "/a", "/a/b"]);
    }
}

/// get-aterm.ps1 hands the Windows verifier a folder such as `C:\Users\me\Temp\x`: one
/// component, as `PurePosixPath` reads it, to which a proof's name is joined after a `/`, a
/// separator Windows reads as `\`.
#[test]
fn a_windows_folder_is_one_component_its_proofs_are_joined_to() {
    let folder = PyPath::new(OsStr::new(r"C:\Users\me\Temp\aterm-get-1"));
    assert_eq!(folder.to_string(), r"C:\Users\me\Temp\aterm-get-1");
    assert_eq!(
        folder.join("aterm-machines.toml").to_path(),
        PathBuf::from(r"C:\Users\me\Temp\aterm-get-1/aterm-machines.toml")
    );
    assert_eq!(
        PyPath::new(OsStr::new("C:/a//b/./")).join("c").to_string(),
        "C:/a/b/c"
    );
}

#[test]
fn a_repr_quotes_as_pythons() {
    assert_eq!(repr(OsStr::new("it's")), "\"it's\"");
    assert_eq!(repr(OsStr::new("a\tb")), "'a\\tb'");
}

#[cfg(unix)]
#[test]
fn an_os_error_reads_as_pythons() {
    use std::os::unix::ffi::OsStrExt as _;
    let error = std::io::Error::from_raw_os_error(libc::ENOENT);
    assert_eq!(
        os_error(&error, Path::new("/no/such")).0,
        "[Errno 2] No such file or directory: '/no/such'"
    );
    assert_eq!(repr(OsStr::from_bytes(b"a\xffb")), "'a\\udcffb'");
}

#[cfg(unix)]
#[test]
fn a_tags_parts_compare_as_pythons_ints() {
    let tuple = |v: &str| int_tuple(v).expect("ints");
    assert!(tuple("0.101.0") >= tuple("0.100.0"));
    assert!(tuple("0.100.0") >= tuple("0.100.0"));
    assert!(tuple("0.99.0") < tuple("0.100.0"));
    assert!(tuple("1.0.0") > tuple("0.999.999"));
    assert!(
        tuple("0.100.0") > tuple("0.100"),
        "a longer tuple of equal prefix"
    );
    assert_eq!(tuple(" 007 "), tuple("7"));
    assert_eq!(tuple("1_0"), tuple("10"));
    assert!(tuple("-1") < tuple("0"));
    assert_eq!(
        int("x").expect_err("not an int").0,
        "invalid literal for int() with base 10: 'x'"
    );
    assert!(int("1__0").is_err() && int("_1").is_err() && int("").is_err());
}

#[test]
fn values_compare_as_pythons() {
    #[cfg(unix)]
    {
        assert!(equals_int(Some(&Value::Integer(1)), 1));
        assert!(equals_int(Some(&Value::Boolean(true)), 1));
        assert!(equals_int(Some(&Value::Float(1.0)), 1));
        assert!(!equals_int(Some(&Value::String("1".into())), 1));
    }
    assert!(!truthy(Some(&Value::Table(aterm_toml::Table::new()))));
    assert!(truthy(Some(&Value::Float(f64::NAN))));
    assert!(uint(Some(&Value::Boolean(true)), "x", u64::MAX).is_err());
    assert_eq!(
        uint(Some(&Value::Integer(-1)), "x", u64::MAX)
            .expect_err("negative")
            .0,
        "invalid x"
    );
}
