// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Value-free views of the names in the applied secrets registry. This module receives
//! names, and the value-free standing of each Claude Code token the vault holds
//! ([`crate::claude_token::Standing`]); it never opens credential files or decrypts their
//! contents.

use crate::registry::{pointer, purpose as description};

/// One Claude Code token entry as `ls` lists it: its name and what it is, never a value and
/// never a file (DESIGN-claude-setup-token §5.10).
pub(super) struct TokenRow {
    name: String,
    description: String,
}

/// The rows of the tokens `standings` names, at `now` (unix seconds): each token entry described
/// with its account, its state, where it was minted and when it ends, and its meta entry as
/// metadata for it. Neither has a file pointer: the class is consumed in place.
pub(super) fn token_rows(standings: &[crate::claude_token::Standing], now: u64) -> Vec<TokenRow> {
    let mut rows = Vec::with_capacity(standings.len() * 2);
    for standing in standings {
        let state = standing.state(now).words();
        let description = match &standing.meta {
            Some(meta) => format!(
                "Claude Code token for {}, consumed in place by aterm's harness: no file; never \
                 export it. {state}; minted on {}, ends {}",
                meta.email,
                meta.minted_on,
                day(meta.expires_at)
            ),
            None => format!(
                "Claude Code token, consumed in place by aterm's harness: no file; never export \
                 it. {state}"
            ),
        };
        rows.push(TokenRow {
            name: standing.key.entry(),
            description,
        });
        rows.push(TokenRow {
            name: standing.key.meta_entry(),
            description: "metadata for that token (no secret)".to_owned(),
        });
    }
    rows
}

/// The UTC day of the unix second `secs`, `YYYY-MM-DD`.
fn day(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub(super) fn listing(
    names: &[String],
    tokens: &[TokenRow],
    describe: bool,
    json: bool,
) -> crate::Result<String> {
    if json {
        let entries: Vec<_> = names
            .iter()
            .map(|name| {
                aterm_json::json!({
                    "name": name,
                    "description": description(name),
                    "pointer": pointer(name),
                })
            })
            .chain(tokens.iter().map(|row| {
                aterm_json::json!({
                    "name": row.name,
                    "description": row.description,
                    "pointer": aterm_json::Value::Null,
                })
            }))
            .collect();
        return aterm_json::to_string(&aterm_json::json!({"entries": entries}))
            .map(|mut text| {
                text.push('\n');
                text
            })
            .map_err(|error| crate::Error::new(error.to_string()));
    }
    let mut out = String::new();
    for name in names {
        if describe {
            out.push_str(&format!(
                "{}\t{}\t{}\n",
                aterm_codec::tty::one_line(name, 512),
                pointer(name).map_or_else(
                    || "registry only".into(),
                    |p| aterm_codec::tty::one_line(&p, 512)
                ),
                aterm_codec::tty::one_line(&description(name), 512)
            ));
        } else {
            out.push_str(name);
            out.push('\n');
        }
    }
    for row in tokens {
        if describe {
            out.push_str(&format!(
                "{}\tno file\t{}\n",
                aterm_codec::tty::one_line(&row.name, 512),
                aterm_codec::tty::one_line(&row.description, 512)
            ));
        } else {
            out.push_str(&row.name);
            out.push('\n');
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;
