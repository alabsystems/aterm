// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared test scaffolding: the repository's own `.toml` corpus, and a
//! comparator between this crate's value tree and the `toml` oracle's.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The repository root, two levels up from this crate's manifest.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-toml sits two levels under the repository root")
        .to_path_buf()
}

/// Every `.toml` file tracked in the repository.
///
/// This corpus IS the specification for the purposes of these tests: real
/// manifests, real vendored manifests, real art assets, the real config files.
/// `target/` and `.git/` are skipped — one is build output, the other is not
/// source.
pub fn corpus() -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&repo_root(), &mut out);
    out.sort();
    assert!(
        out.len() > 100,
        "the corpus should be the whole tree, found only {}",
        out.len()
    );
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "target" || name == ".git" || name == "node_modules" {
                continue;
            }
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "toml") {
            out.push(path);
        }
    }
}

/// Compare this crate's parse of a document against the `toml` oracle's.
///
/// Floats are compared bitwise after normalizing NaN, because `0.0 == -0.0` is
/// true and a serializer that swapped them would slip through a `==`.
pub fn values_agree(ours: &aterm_toml::Value, theirs: &toml::Value) -> Result<(), String> {
    match (ours, theirs) {
        (aterm_toml::Value::String(a), toml::Value::String(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!("string {a:?} != {b:?}"))
            }
        }
        (aterm_toml::Value::Integer(a), toml::Value::Integer(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!("integer {a} != {b}"))
            }
        }
        (aterm_toml::Value::Float(a), toml::Value::Float(b)) => {
            let same = if a.is_nan() && b.is_nan() {
                a.is_sign_negative() == b.is_sign_negative()
            } else {
                a.to_bits() == b.to_bits()
            };
            if same {
                Ok(())
            } else {
                Err(format!("float {a} != {b}"))
            }
        }
        (aterm_toml::Value::Boolean(a), toml::Value::Boolean(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!("boolean {a} != {b}"))
            }
        }
        (aterm_toml::Value::Datetime(a), toml::Value::Datetime(b)) => {
            let (a, b) = (a.to_string(), b.to_string());
            if a.eq_ignore_ascii_case(&b) {
                Ok(())
            } else {
                Err(format!("datetime {a} != {b}"))
            }
        }
        (aterm_toml::Value::Array(a), toml::Value::Array(b)) => {
            if a.len() != b.len() {
                return Err(format!("array length {} != {}", a.len(), b.len()));
            }
            for (index, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                values_agree(x, y).map_err(|e| format!("[{index}]: {e}"))?;
            }
            Ok(())
        }
        (aterm_toml::Value::Table(a), toml::Value::Table(b)) => {
            let ours: Vec<&str> = a.keys().map(String::as_str).collect();
            let theirs: Vec<&str> = b.keys().map(String::as_str).collect();
            if ours != theirs {
                return Err(format!("table keys {ours:?} != {theirs:?}"));
            }
            for (key, x) in a.iter() {
                let y = b.get(key).expect("key sets already compared equal");
                values_agree(x, y).map_err(|e| format!(".{key}: {e}"))?;
            }
            Ok(())
        }
        _ => Err(format!("kind {} != {}", ours.type_str(), theirs.type_str())),
    }
}

/// The ONE place this crate deliberately disagrees with the `toml` oracle.
///
/// A finite float literal that overflows binary64 — `1e400` — is refused here,
/// in BOTH signs. `toml` 0.8.23 refuses only the positive one; measured on this
/// tree:
///
/// ```text
/// x = 1e400   => Err("invalid floating-point number")
/// x = -1e400  => Ok(Float(-inf))
/// ```
///
/// That asymmetry is an upstream slip, and copying it would mean a config that
/// reads `timeout = -1e400` silently becomes negative infinity while the same
/// typo with a positive sign is caught. Rejecting both is fail-closed and
/// consistent, so the differential fuzz skips a case where the oracle saturated
/// a float the source spelled as a finite literal.
///
/// The evidence looked for is the OVERFLOWING LITERAL itself, not the absence
/// of the substring `inf` anywhere in the document. A generated case that holds
/// both a real `inf` and a `9223372e36854775808` is legal TOML for the oracle
/// and rejected here, and the substring test called that a divergence.
pub fn oracle_saturated_a_float(value: &toml::Value, source: &str) -> bool {
    fn has_infinity(value: &toml::Value) -> bool {
        match value {
            toml::Value::Float(f) => f.is_infinite(),
            toml::Value::Array(a) => a.iter().any(has_infinity),
            toml::Value::Table(t) => t.values().any(has_infinity),
            _ => false,
        }
    }
    if !has_infinity(value) {
        return false;
    }
    // `inf` and `nan` cannot appear in a token drawn from this alphabet, so a
    // run that parses to infinity is necessarily a finite literal that
    // saturated — the exact shape this crate refuses in both signs.
    source
        .split(|c: char| !matches!(c, '0'..='9' | 'e' | 'E' | '+' | '-' | '.' | '_'))
        .any(|token| {
            token
                .replace('_', "")
                .parse::<f64>()
                .is_ok_and(f64::is_infinite)
        })
}
