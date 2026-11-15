// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The open-work reader's three corpora, each read by its one parser from the text the test
//! includes: the labelled tests judge these rows, and the reader's measurements
//! (`open_work_measure_tests.rs`) read the same ones.

/// The reports of `corpus` (`open-work-corpus.txt`'s text), in order, each named as `corpus`
/// names it and with its excerpts: whether the reader reads the excerpt as open (its label, or the
/// reading its mark says it misses or flags), and its text.
pub fn open_work_reports(corpus: &str) -> Vec<(&str, Vec<(bool, String)>)> {
    let mut reports: Vec<(&str, Vec<(bool, String)>)> = Vec::new();
    let mut excerpt: Option<(bool, String)> = None;
    for line in corpus.lines().skip_while(|l| !l.starts_with("=== ")) {
        if (line.starts_with("=== ") || line.starts_with("--- "))
            && let (Some(done), Some((_, excerpts))) = (excerpt.take(), reports.last_mut())
        {
            excerpts.push(done);
        }
        if let Some(name) = line.strip_prefix("=== ") {
            reports.push((name, Vec::new()));
        } else if let Some(mark) = line.strip_prefix("--- ") {
            let (label, known) = mark.split_once(", ").unwrap_or((mark, ""));
            let open = match label {
                "open" => true,
                "clear" => false,
                _ => panic!("no label: {mark:?}"),
            };
            let reads_open = match known.split_once(':').map(|(k, _)| k) {
                None => open,
                Some("missed") if open => false,
                Some("flagged") if !open => true,
                _ => panic!("a mark that contradicts its label: {mark:?}"),
            };
            excerpt = Some((reads_open, String::new()));
        } else if let Some((_, text)) = excerpt.as_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    if let (Some(done), Some((_, excerpts))) = (excerpt, reports.last_mut()) {
        excerpts.push(done);
    }
    reports
}

/// The rows of `text` (`land-defense-table.txt`'s), in order: whether the row is labelled open,
/// and its reply.
pub fn land_table(text: &str) -> Vec<(bool, String)> {
    let mut rows = Vec::new();
    let mut open = None;
    for line in text.lines() {
        match line {
            "== open" => open = Some(true),
            "== clean" => open = Some(false),
            _ if line.starts_with('#') || line.trim().is_empty() => {}
            _ => rows.push((
                open.unwrap_or_else(|| panic!("a row before its section: {line:?}")),
                line.replace("\\n", "\n"),
            )),
        }
    }
    rows
}

/// A holdout's cases (`land-holdout-2026-10-20.tsv`'s shape), in order: label, id, provenance
/// and reply.
pub fn holdout_cases(text: &str) -> Vec<(&str, &str, &str, String)> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|line| {
            let cols: Vec<&str> = line.splitn(5, '\t').collect();
            let [label, id, prov, _, reply] = cols[..] else {
                panic!("a case needs five columns: {line:?}");
            };
            (label, id, prov, reply.replace("\\n", "\n"))
        })
        .collect()
}

/// Every reply the three corpora hold, each with its corpus and its row there, from their texts:
/// `open-work-corpus.txt`'s, `land-defense-table.txt`'s and the committed holdout's
/// (`land-holdout-2026-10-20.tsv`).
pub fn corpus_rows(
    open_work: &str,
    land_defense: &str,
    holdout: &str,
) -> Vec<(&'static str, usize, String)> {
    let mut rows: Vec<(&'static str, usize, String)> = Vec::new();
    let excerpts = open_work_reports(open_work)
        .into_iter()
        .flat_map(|(_, e)| e);
    for (n, (_, text)) in excerpts.enumerate() {
        rows.push(("open-work-corpus", n, text));
    }
    for (n, (_, reply)) in land_table(land_defense).into_iter().enumerate() {
        rows.push(("land-defense-table", n, reply));
    }
    for (n, (.., reply)) in holdout_cases(holdout).into_iter().enumerate() {
        rows.push(("land-holdout-2026-10-20", n, reply));
    }
    rows
}
