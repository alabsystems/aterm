// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compares the linker symbol bound by each `extern` declaration against the reference.

use std::collections::BTreeMap;
use std::fmt;

use crate::rustdoc::{Namespace, Public, Result};
use crate::shape::function_has_body;
use aterm_json::Value;

/// Returns the `link_name` an item's attributes give it, if any.
#[must_use]
pub fn link_name(item: &Value) -> Option<String> {
    let attrs = item.get("attrs")?.as_array()?;
    attrs.iter().find_map(|attr| {
        let text = match attr {
            Value::String(text) => text.as_str(),
            Value::Object(_) => attr.get("other")?.as_str()?,
            _ => return None,
        };
        if !(text.contains("LinkName") || text.contains("link_name")) {
            return None;
        }
        let (_, quoted) = text.split_once('"')?;
        let (name, _) = quoted.split_once('"')?;
        Some(name.to_string())
    })
}

/// Maps each crate-root `extern` function name to the symbol it binds.
///
/// # Errors
/// When rustdoc omitted a function's body bit.
pub fn extern_symbols(public: &Public<'_>) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (name, by_namespace) in public {
        let Some(entry) = by_namespace.get(&Namespace::Value) else {
            continue;
        };
        if entry.kind != "function" || function_has_body(name, entry.item)? {
            continue;
        }
        let symbol = link_name(entry.item).unwrap_or_else(|| (*name).to_string());
        out.insert((*name).to_string(), symbol);
    }
    Ok(out)
}

/// A declaration the shim binds differently from the reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolDiff {
    /// The shim declares an extern the reference does not have on this cell.
    AbsentFromReference { name: String },
    /// Both declare it; the symbols differ.
    Mismatch {
        name: String,
        shim: String,
        reference: String,
    },
}

impl fmt::Display for SymbolDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AbsentFromReference { name } => {
                write!(
                    f,
                    "{name}: declared by the shim, absent from libc on this cell"
                )
            }
            Self::Mismatch {
                name,
                shim,
                reference,
            } => write!(f, "{name}: shim binds `{shim}`, libc binds `{reference}`"),
        }
    }
}

/// Every shim declaration whose symbol is not the reference's.
#[must_use]
pub fn diff(
    reference: &BTreeMap<String, String>,
    shim: &BTreeMap<String, String>,
) -> Vec<SymbolDiff> {
    shim.iter()
        .filter_map(|(name, symbol)| match reference.get(name) {
            None => Some(SymbolDiff::AbsentFromReference { name: name.clone() }),
            Some(expected) if expected != symbol => Some(SymbolDiff::Mismatch {
                name: name.clone(),
                shim: symbol.clone(),
                reference: expected.clone(),
            }),
            Some(_) => None,
        })
        .collect()
}

/// How many declarations bind a symbol other than their own name.
#[must_use]
pub fn redirected(symbols: &BTreeMap<String, String>) -> usize {
    symbols
        .iter()
        .filter(|(name, symbol)| name != symbol)
        .count()
}

#[cfg(test)]
#[path = "symbols_tests.rs"]
mod tests;
