// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sitemap.xml`: ensures `/releases` has an entry and updates `lastmod` on changed pages.

/// The site origin the sitemap's `<loc>`s are under.
pub const ORIGIN: &str = "https://alab.systems";

/// One `<url>` block: `  <url>\n    <loc>LOC</loc>\n    <lastmod>DATE</lastmod>\n…  </url>\n`.
struct Block {
    start: usize,
    end: usize,
    loc: (usize, usize),
    lastmod: (usize, usize),
}

fn blocks(text: &str) -> Vec<Block> {
    const OPEN: &str = "  <url>\n    <loc>";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(OPEN) {
        let start = from + at;
        let parsed = (|| {
            let loc_start = start + OPEN.len();
            let loc_end = loc_start + text[loc_start..].find('<')?;
            let mid = text[loc_end..].strip_prefix("</loc>\n    <lastmod>")?;
            let mod_start = text.len() - mid.len();
            let mod_end = mod_start + text[mod_start..].find('<')?;
            let tail = text[mod_end..].strip_prefix("</lastmod>\n")?;
            let tail_start = text.len() - tail.len();
            let close = tail_start + text[tail_start..].find("  </url>\n")?;
            Some(Block {
                start,
                end: close + "  </url>\n".len(),
                loc: (loc_start, loc_end),
                lastmod: (mod_start, mod_end),
            })
        })();
        match parsed {
            Some(b) => {
                from = b.end;
                out.push(b);
            }
            None => from = start + 1,
        }
    }
    out
}

/// Whether `s` is `YYYY-MM-DD`.
#[must_use]
pub fn is_date(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(k, b)| {
            if k == 4 || k == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// What [`update`] did beside the dates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    pub text: String,
    /// `ensure`'s entry was added.
    pub added: bool,
}

/// Updates `lastmod` to `today` for `touch` pages and adds `ensure` when absent.
/// # Errors
/// Invalid `today` date, no `<url>` block of the expected shape, or missing `</urlset>`.
pub fn update(text: &str, ensure: &str, today: &str, touch: &[&str]) -> Result<Updated, String> {
    if !is_date(today) {
        return Err(format!(
            "the sitemap date must be YYYY-MM-DD, got {today:?}"
        ));
    }
    let found = blocks(text);
    if found.is_empty() {
        return Err(
            "sitemap.xml does not have the <url>/<loc>/<lastmod> shape this rule expects".into(),
        );
    }
    let mut out = String::new();
    let mut last = 0;
    for b in &found {
        let loc = &text[b.loc.0..b.loc.1];
        out.push_str(&text[last..b.lastmod.0]);
        let touched = loc
            .strip_prefix(ORIGIN)
            .is_some_and(|rest| touch.contains(&rest));
        out.push_str(if touched {
            today
        } else {
            &text[b.lastmod.0..b.lastmod.1]
        });
        out.push_str(&text[b.lastmod.1..b.end]);
        last = b.end;
        debug_assert!(b.start <= b.lastmod.0);
    }
    out.push_str(&text[last..]);
    let ensure_loc = format!("<loc>{ORIGIN}{ensure}</loc>");
    if out.contains(&ensure_loc) {
        return Ok(Updated {
            text: out,
            added: false,
        });
    }
    let entry = format!(
        "  <url>\n    {ensure_loc}\n    <lastmod>{today}</lastmod>\n    \
         <changefreq>weekly</changefreq>\n    <priority>0.6</priority>\n  </url>\n"
    );
    let Some(at) = out.find("</urlset>") else {
        return Err("sitemap.xml has no </urlset>".into());
    };
    out.insert_str(at, &entry);
    Ok(Updated {
        text: out,
        added: true,
    })
}

/// Every block's `(loc, lastmod)`, in order.
#[cfg(test)]
pub(crate) fn lastmods(text: &str) -> Vec<(String, String)> {
    blocks(text)
        .iter()
        .map(|b| {
            (
                text[b.loc.0..b.loc.1].to_string(),
                text[b.lastmod.0..b.lastmod.1].to_string(),
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "sitemap_tests.rs"]
mod tests;
