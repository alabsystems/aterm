// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Output formatting for `--format`.

use aterm_json::Value;

use super::args::Format;
use super::project::{FieldPath, value_at};

/// Render projected values in the selected [`Format`].
#[must_use]
pub fn render(
    values: &[Value],
    is_list: bool,
    format: Format,
    header: bool,
    columns: &[FieldPath],
) -> String {
    match format {
        Format::Json => {
            let doc = if is_list {
                Value::Array(values.to_vec())
            } else {
                values.first().cloned().unwrap_or(Value::Null)
            };
            format!("{doc}\n")
        }
        Format::Ndjson => values.iter().map(|value| format!("{value}\n")).collect(),
        Format::Tsv => {
            let mut out = String::new();
            if header {
                let texts: Vec<String> = columns.iter().map(|c| tsv_escape(&c.text())).collect();
                out.push_str(&texts.join("\t"));
                out.push('\n');
            }
            for value in values {
                let cells: Vec<String> = columns
                    .iter()
                    .map(|c| tsv_escape(&cell(&value_at(value, c))))
                    .collect();
                out.push_str(&cells.join("\t"));
                out.push('\n');
            }
            out
        }
        Format::Text => {
            let mut rows: Vec<Vec<String>> = Vec::with_capacity(values.len() + 1);
            rows.push(columns.iter().map(|c| for_person(&c.text())).collect());
            for value in values {
                rows.push(
                    columns
                        .iter()
                        .map(|c| for_person(&cell(&value_at(value, c))))
                        .collect(),
                );
            }
            let mut widths = vec![0usize; columns.len()];
            for row in &rows {
                for (width, text) in widths.iter_mut().zip(row) {
                    *width = (*width).max(text.chars().count());
                }
            }
            let mut out = String::new();
            for row in &rows {
                let mut line = String::new();
                for (i, (text, width)) in row.iter().zip(&widths).enumerate() {
                    if i > 0 {
                        line.push_str("  ");
                    }
                    line.push_str(text);
                    let pad = width.saturating_sub(text.chars().count());
                    line.extend(std::iter::repeat_n(' ', pad));
                }
                out.push_str(line.trim_end_matches(' '));
                out.push('\n');
            }
            out
        }
    }
}

/// Formats a single cell value as an unescaped string.
pub(crate) fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(cell).collect::<Vec<_>>().join(","),
        Value::Bool(_) | Value::Number(_) | Value::Object(_) => value.to_string(),
    }
}

/// Escapes `\`, `\t`, `\n`, `\r`, and NUL for `@tsv` output.
fn tsv_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out
}

/// Escapes tabs, line endings, and control bytes for terminal display.
fn for_person(text: &str) -> String {
    let mut flat = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\n' | '\r' | '\t') {
            aterm_codec::tty::push_escape(&mut flat, c);
        } else {
            flat.push(c);
        }
    }
    aterm_codec::tty::safe(flat.as_bytes()).into_owned()
}
