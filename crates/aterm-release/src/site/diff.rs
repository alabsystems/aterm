// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A dry run's view of a planned page: a unified line diff, three lines of context.

/// The lines of `new` that differ from `old`, as `-`/`+`/` ` rows in hunks headed
/// `@@ -a,n +b,m @@`, under a `--- path` / `+++ path (planned)` header.
#[must_use]
pub fn unified(path: &str, old: &str, new: &str) -> Vec<String> {
    if old == new {
        return Vec::new();
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let am = &a[prefix..];
    let bm = &b[prefix..];
    let width = bm.len() + 1;
    let mut lcs = vec![0u32; (am.len() + 1) * width];
    for i in (0..am.len()).rev() {
        for j in (0..bm.len()).rev() {
            lcs[i * width + j] = if am[i] == bm[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut script = Vec::with_capacity(a.len() + bm.len());
    for k in 0..prefix {
        script.push((' ', k, k));
    }
    let (mut i, mut j) = (0, 0);
    while i < am.len() || j < bm.len() {
        if i < am.len() && j < bm.len() && am[i] == bm[j] {
            script.push((' ', prefix + i, prefix + j));
            i += 1;
            j += 1;
        } else if i < am.len()
            && (j == bm.len() || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
        {
            script.push(('-', prefix + i, prefix + j));
            i += 1;
        } else {
            script.push(('+', prefix + i, prefix + j));
            j += 1;
        }
    }
    const CONTEXT: usize = 3;
    let changed: Vec<usize> = (0..script.len()).filter(|k| script[*k].0 != ' ').collect();
    let mut out = Vec::new();
    if changed.is_empty() {
        return out;
    }
    out.push(format!("--- {path}"));
    out.push(format!("+++ {path} (planned)"));
    let mut k = 0;
    while k < changed.len() {
        let first = changed[k].saturating_sub(CONTEXT);
        let mut last = changed[k];
        while k + 1 < changed.len() && changed[k + 1] <= last + 2 * CONTEXT + 1 {
            k += 1;
            last = changed[k];
        }
        let last = (last + CONTEXT).min(script.len() - 1);
        let rows = &script[first..=last];
        let old_n = rows.iter().filter(|r| r.0 != '+').count();
        let new_n = rows.iter().filter(|r| r.0 != '-').count();
        out.push(format!(
            "@@ -{},{old_n} +{},{new_n} @@",
            rows[0].1 + 1,
            rows[0].2 + 1
        ));
        for (kind, i, j) in rows {
            let line = if *kind == '+' { b[*j] } else { a[*i] };
            out.push(format!("{kind}{line}"));
        }
        k += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_changed_lines_and_their_context_are_shown() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
        assert_eq!(
            unified("p", old, new),
            [
                "--- p",
                "+++ p (planned)",
                "@@ -2,8 +2,9 @@",
                " b",
                " c",
                " d",
                "-e",
                "+E",
                " f",
                " g",
                " h",
                " i",
                "+j",
            ]
        );
        assert!(unified("p", old, old).is_empty());
    }
}
