// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Python value, path, integer, and `OSError` semantics used by the embedded verifier.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use aterm_toml::Value;

use crate::{Refusal, Verdict};

/// Parses `value` as an integer in `0..=maximum`, else refuses with `invalid <name>`.
pub(crate) fn uint(value: Option<&Value>, name: &str, maximum: u64) -> Verdict<u64> {
    match value {
        Some(Value::Integer(v)) => u64::try_from(*v)
            .ok()
            .filter(|v| *v <= maximum)
            .ok_or_else(|| Refusal::new(format!("invalid {name}"))),
        _ => Err(Refusal::new(format!("invalid {name}"))),
    }
}

/// [`uint`] of a table's `key`, `default` when it is absent (`table.get(key, default)`).
pub(crate) fn uint_or(
    value: Option<&Value>,
    default: i64,
    name: &str,
    maximum: u64,
) -> Verdict<u64> {
    match value {
        Some(value) => uint(Some(value), name, maximum),
        None => uint(Some(&Value::Integer(default)), name, maximum),
    }
}

/// Python's `bool(value)`, an absent key being `None`.
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Integer(i)) => *i != 0,
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Boolean(b)) => *b,
        Some(Value::Datetime(_)) => true,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Table(t)) => !t.is_empty(),
    }
}

/// Python's `value == n`: an int, a bool (`True == 1`) or a float equal to it.
pub(crate) fn equals_int(value: Option<&Value>, n: i64) -> bool {
    match value {
        Some(Value::Integer(v)) => *v == n,
        Some(Value::Boolean(b)) => i64::from(*b) == n,
        Some(Value::Float(f)) => i32::try_from(n).is_ok_and(|n| *f == f64::from(n)),
        _ => false,
    }
}

/// Python's `value == text`: only a string equals a string.
pub(crate) fn equals_str(value: Option<&Value>, text: &str) -> bool {
    value.and_then(Value::as_str) == Some(text)
}

/// Python's `repr()` of an OS byte string using `surrogateescape` for undecodable bytes.
pub(crate) fn repr(text: &OsStr) -> String {
    let mut body = String::new();
    let mut quote = '\'';
    let mut has_single = false;
    let mut has_double = false;
    for chunk in text.as_bytes().utf8_chunks() {
        has_single |= chunk.valid().contains('\'');
        has_double |= chunk.valid().contains('"');
    }
    if has_single && !has_double {
        quote = '"';
    }
    for chunk in text.as_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => body.push_str("\\\\"),
                '\n' => body.push_str("\\n"),
                '\r' => body.push_str("\\r"),
                '\t' => body.push_str("\\t"),
                c if c == quote => {
                    body.push('\\');
                    body.push(c);
                }
                c if u32::from(c) < 0x20 || (0x7f..0xa0).contains(&u32::from(c)) => {
                    body.push_str(&format!("\\x{:02x}", u32::from(c)));
                }
                c => body.push(c),
            }
        }
        for byte in chunk.invalid() {
            body.push_str(&format!("\\udc{byte:02x}"));
        }
    }
    format!("{quote}{body}{quote}")
}

fn strerror(error: &std::io::Error) -> String {
    let text = error.to_string();
    match error.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .unwrap_or(&text)
            .to_string(),
        None => text,
    }
}

/// Formats an `OSError` for `path` as `[Errno N] <msg>: '<path>'`.
pub(crate) fn os_error(error: &std::io::Error, path: &Path) -> Refusal {
    match error.raw_os_error() {
        Some(code) => Refusal::new(format!(
            "[Errno {code}] {}: {}",
            strerror(error),
            repr(path.as_os_str())
        )),
        None => Refusal::new(format!("{error}: {}", repr(path.as_os_str()))),
    }
}

/// An `OSError` with no file named (a read that failed on an open file).
pub(crate) fn bare_os_error(error: &std::io::Error) -> Refusal {
    match error.raw_os_error() {
        Some(code) => Refusal::new(format!("[Errno {code}] {}", strerror(error))),
        None => Refusal::new(error.to_string()),
    }
}

/// A POSIX path normalized as `pathlib.PurePosixPath` spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PyPath {
    root: &'static str,
    parts: Vec<OsString>,
}

impl PyPath {
    pub(crate) fn new(text: &OsStr) -> Self {
        let bytes = text.as_bytes();
        let root = if bytes.starts_with(b"//") && !bytes.starts_with(b"///") {
            "//"
        } else if bytes.starts_with(b"/") {
            "/"
        } else {
            ""
        };
        let parts = bytes
            .split(|b| *b == b'/')
            .filter(|part| !part.is_empty() && *part != b".")
            .map(|part| OsString::from_vec(part.to_vec()))
            .collect();
        Self { root, parts }
    }

    /// `Path.absolute()`: a relative path under the working directory.
    pub(crate) fn absolute(self) -> Verdict<Self> {
        if !self.root.is_empty() {
            return Ok(self);
        }
        let cwd = std::env::current_dir().map_err(|e| bare_os_error(&e))?;
        let mut base = Self::new(cwd.as_os_str());
        base.parts.extend(self.parts);
        Ok(base)
    }

    /// `self / name`.
    pub(crate) fn join(&self, name: &str) -> Self {
        let mut joined = self.clone();
        joined.parts.extend(Self::new(OsStr::new(name)).parts);
        joined
    }

    /// `self.parent`: the path itself for a root or an empty relative path.
    pub(crate) fn parent(&self) -> Self {
        let mut parent = self.clone();
        parent.parts.pop();
        parent
    }

    /// `reversed((self, *self.parents))`: the root (or the first relative component)
    /// down to the path itself.
    pub(crate) fn lineage(&self) -> Vec<Self> {
        (0..=self.parts.len())
            .map(|n| Self {
                root: self.root,
                parts: self.parts[..n].to_vec(),
            })
            .filter(|path| {
                !(path.root.is_empty() && path.parts.is_empty()) || self.parts.is_empty()
            })
            .collect()
    }

    /// The path to hand the OS.
    pub(crate) fn to_path(&self) -> PathBuf {
        PathBuf::from(self.to_os())
    }

    fn to_os(&self) -> OsString {
        let mut out = OsString::from(self.root);
        for (n, part) in self.parts.iter().enumerate() {
            if n > 0 {
                out.push("/");
            }
            out.push(part);
        }
        if out.is_empty() {
            out.push(".");
        }
        out
    }

    /// `str(path) == text`.
    pub(crate) fn is(&self, text: &str) -> bool {
        self.to_os() == OsStr::new(text)
    }
}

impl fmt::Display for PyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_os().to_string_lossy())
    }
}

/// An `int()` of a tag's part: its sign and its digits, leading zeros gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Int {
    negative: bool,
    digits: String,
}

impl Ord for Int {
    fn cmp(&self, other: &Self) -> Ordering {
        let magnitude = self
            .digits
            .len()
            .cmp(&other.digits.len())
            .then_with(|| self.digits.cmp(&other.digits));
        match (self.negative, other.negative) {
            (false, false) => magnitude,
            (true, true) => magnitude.reverse(),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }
}

impl PartialOrd for Int {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Parses `text` as Python's base-10 `int(text)`.
pub(crate) fn int(text: &str) -> Verdict<Int> {
    let invalid = || {
        Refusal::new(format!(
            "invalid literal for int() with base 10: {}",
            repr(OsStr::new(text))
        ))
    };
    let trimmed = text.trim();
    let (negative, body) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let bytes = body.as_bytes();
    let well_formed = !bytes.is_empty()
        && bytes.first().is_some_and(u8::is_ascii_digit)
        && bytes.last().is_some_and(u8::is_ascii_digit)
        && bytes.iter().all(|b| b.is_ascii_digit() || *b == b'_')
        && !body.contains("__");
    if !well_formed {
        return Err(invalid());
    }
    let digits: String = body.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_start_matches('0').to_string();
    Ok(Int {
        negative: negative && !digits.is_empty(),
        digits,
    })
}

/// `tuple(map(int, version.split(".")))`.
pub(crate) fn int_tuple(version: &str) -> Verdict<Vec<Int>> {
    version.split('.').map(int).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_spelt_as_pathlib_spells_it() {
        let spelt = |text: &str| PyPath::new(OsStr::new(text)).to_string();
        assert_eq!(spelt("/a//b/./c/"), "/a/b/c");
        assert_eq!(spelt("//a/b"), "//a/b");
        assert_eq!(spelt("///a"), "/a");
        assert_eq!(spelt("a/../b"), "a/../b");
        assert_eq!(spelt(""), ".");
        let lineage: Vec<String> = PyPath::new(OsStr::new("/a/b"))
            .lineage()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(lineage, ["/", "/a", "/a/b"]);
    }

    #[test]
    fn an_os_error_reads_as_pythons() {
        let error = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            os_error(&error, Path::new("/no/such")).0,
            "[Errno 2] No such file or directory: '/no/such'"
        );
        assert_eq!(repr(OsStr::new("it's")), "\"it's\"");
        assert_eq!(repr(OsStr::from_bytes(b"a\xffb")), "'a\\udcffb'");
    }

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
        assert!(equals_int(Some(&Value::Integer(1)), 1));
        assert!(equals_int(Some(&Value::Boolean(true)), 1));
        assert!(equals_int(Some(&Value::Float(1.0)), 1));
        assert!(!equals_int(Some(&Value::String("1".into())), 1));
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
}
