// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE LANDING'S STATED RECEIPT (design B10, `docs/DESIGN-fast-build-release-2026-11-05.md`
//! §4.6): the trailers `targo --unverified verify land` writes on the merge it pushes to main, and
//! their one reader. The writer (aterm-verify's `land`) and the reader (the harness's land
//! receipt) share this module, so the two cannot drift. A trailer states what ran; nothing
//! refuses a commit that lacks one.
//!
//! ```text
//! Aterm-Checked: <tree> rows=<check>:<word>,… base=<commit> secs=<n>
//! Aterm-Composed-Onto: <commit>
//! Aterm-Inherited: <red item id>
//! ```
//!
//! One `Aterm-Checked` line per judgment, in the order they ran: the first judged the branch over
//! the cone since the main commit it merged (`base`), and each re-judge the delta a later merge of
//! main brought, since the commit judged before it. `Aterm-Composed-Onto` names the main commit
//! the last judgment was composed onto without a run; `Aterm-Inherited` is one red item main
//! already had outside the branch's crates per line.

/// One judgment's trailer key.
pub const CHECKED: &str = "Aterm-Checked";
/// The trailer key naming the main commit a judged tree was composed onto.
pub const COMPOSED_ONTO: &str = "Aterm-Composed-Onto";
/// The trailer key naming one red item the landing inherited from main.
pub const INHERITED: &str = "Aterm-Inherited";
/// Every key, in the order a landing writes them (for git's `%(trailers:key=…)`).
pub const KEYS: [&str; 3] = [CHECKED, COMPOSED_ONTO, INHERITED];

/// The outcome words a judgment's row may carry: a landing pushes only after every check passed,
/// skipped, or failed on reds it inherited.
pub const WORDS: [&str; 3] = ["ok", "skip", "FAIL"];

/// One check's row in a judgment: its name (`test`) and its outcome word ([`WORDS`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Row {
    /// The check's name, as `verify list` names it.
    pub check: String,
    /// Its outcome word.
    pub word: String,
}

/// One judgment: the tree the dev set ran on, each check's row, the commit its cone was computed
/// from, and how long it ran.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Checked {
    /// The judged tree's object id.
    pub tree: String,
    /// Each check's row, in the order the run finished them.
    pub rows: Vec<Row>,
    /// The commit the run's cone was computed from (`verify --base`).
    pub base: String,
    /// The run's wall seconds.
    pub secs: u64,
}

/// A landing merge's stated receipt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stated {
    /// Every judgment, in the order they ran; never empty in a parsed receipt.
    pub checked: Vec<Checked>,
    /// The main commit the last judgment was composed onto, where main moved after it.
    pub composed_onto: Option<String>,
    /// The red items the landing inherited, each named once.
    pub inherited: Vec<String>,
}

impl Checked {
    /// Its trailer value: `<tree> rows=<check>:<word>,… base=<commit> secs=<n>`.
    #[must_use]
    pub fn value(&self) -> String {
        let rows: Vec<String> = self
            .rows
            .iter()
            .map(|row| format!("{}:{}", row.check, row.word))
            .collect();
        format!(
            "{} rows={} base={} secs={}",
            self.tree,
            rows.join(","),
            self.base,
            self.secs
        )
    }

    /// The judgment a trailer `value` states; `None` unless every part has its shape.
    #[must_use]
    pub fn of_value(value: &str) -> Option<Self> {
        let mut parts = value.split(' ');
        let tree = parts.next().filter(|id| object_id(id))?;
        let rows = parts.next()?.strip_prefix("rows=")?;
        let base = parts
            .next()?
            .strip_prefix("base=")
            .filter(|id| object_id(id))?;
        let secs = parts.next()?.strip_prefix("secs=")?;
        if parts.next().is_some() || !secs.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let rows = if rows.is_empty() {
            Vec::new()
        } else {
            rows.split(',')
                .map(|row| {
                    let (check, word) = row.split_once(':')?;
                    let named = !check.is_empty()
                        && check
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
                    (named && WORDS.contains(&word)).then(|| Row {
                        check: check.to_owned(),
                        word: word.to_owned(),
                    })
                })
                .collect::<Option<Vec<_>>>()?
        };
        Some(Self {
            tree: tree.to_owned(),
            rows,
            base: base.to_owned(),
            secs: secs.parse().ok()?,
        })
    }
}

impl Stated {
    /// Its trailer block, one `Key: value` line each (no blank line before it).
    #[must_use]
    pub fn trailers(&self) -> String {
        let mut block = String::new();
        for checked in &self.checked {
            block.push_str(&format!("{CHECKED}: {}\n", checked.value()));
        }
        if let Some(onto) = &self.composed_onto {
            block.push_str(&format!("{COMPOSED_ONTO}: {onto}\n"));
        }
        for item in &self.inherited {
            block.push_str(&format!("{INHERITED}: {item}\n"));
        }
        block
    }

    /// The judgments after the first: how often a moved main was merged and judged again.
    #[must_use]
    pub fn rejudges(&self) -> usize {
        self.checked.len().saturating_sub(1)
    }

    /// The receipt the trailer lines in `text` state (git's `%(trailers:…)` output, or a
    /// message's trailer block). Lines with other keys are passed over. `None` where no
    /// `Aterm-Checked` line is present, or any line with one of [`KEYS`] is malformed: a receipt
    /// is read whole or not at all.
    #[must_use]
    pub fn of_trailers(text: &str) -> Option<Self> {
        let mut stated = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(": ") else {
                continue;
            };
            match key {
                CHECKED => stated.checked.push(Checked::of_value(value)?),
                COMPOSED_ONTO if object_id(value) && stated.composed_onto.is_none() => {
                    stated.composed_onto = Some(value.to_owned());
                }
                COMPOSED_ONTO => return None,
                INHERITED if one_line(value) && !stated.inherited.iter().any(|i| i == value) => {
                    stated.inherited.push(value.to_owned());
                }
                INHERITED => return None,
                _ => {}
            }
        }
        (!stated.checked.is_empty()).then_some(stated)
    }
}

/// Whether `id` is a git object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
#[must_use]
pub fn object_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64)
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `value` is a non-empty single line with no control character and no surrounding space.
fn one_line(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    fn judged(tree: char, base: char) -> Checked {
        Checked {
            tree: id(tree),
            rows: vec![
                Row {
                    check: "fmt".into(),
                    word: "ok".into(),
                },
                Row {
                    check: "export-scan".into(),
                    word: "skip".into(),
                },
                Row {
                    check: "test".into(),
                    word: "FAIL".into(),
                },
            ],
            base: id(base),
            secs: 2712,
        }
    }

    /// A receipt reads back as written: two judgments in order, the composed-onto commit and
    /// each inherited item, with other trailers and prose passed over.
    #[test]
    fn a_stated_receipt_reads_back_as_written() {
        let stated = Stated {
            checked: vec![judged('a', 'b'), judged('c', 'd')],
            composed_onto: Some(id('e')),
            inherited: vec!["-p other --lib -- flaky_case".into()],
        };
        let block = stated.trailers();
        assert_eq!(
            block.lines().next(),
            Some(format!(
                "Aterm-Checked: {} rows=fmt:ok,export-scan:skip,test:FAIL base={} secs=2712",
                id('a'),
                id('b')
            ))
            .as_deref()
        );
        let text = format!("Signed-off-by: someone\n{block}Reviewed: yes\n");
        assert_eq!(Stated::of_trailers(&text), Some(stated.clone()));
        assert_eq!(stated.rejudges(), 1);
    }

    /// NEGATIVE CONTROLS: no `Aterm-Checked` line is no receipt, and one malformed line of any
    /// key spoils the whole receipt rather than reading as part of one.
    #[test]
    fn a_receipt_is_read_whole_or_not_at_all() {
        let good = format!("{CHECKED}: {}\n", judged('a', 'b').value());
        assert!(Stated::of_trailers(&good).is_some());
        assert_eq!(Stated::of_trailers(""), None);
        assert_eq!(
            Stated::of_trailers(&format!("{COMPOSED_ONTO}: {}\n", id('e'))),
            None,
            "no judgment"
        );
        for bad in [
            format!("{CHECKED}: {} rows=fmt:ok base={} secs=1", id('A'), id('b')),
            format!(
                "{CHECKED}: {} rows=fmt:green base={} secs=1",
                id('a'),
                id('b')
            ),
            format!("{CHECKED}: {} rows=Fmt:ok base={} secs=1", id('a'), id('b')),
            format!("{CHECKED}: {} rows=fmt:ok base={} secs=x", id('a'), id('b')),
            format!("{CHECKED}: {} rows=fmt:ok base={}", id('a'), id('b')),
            format!(
                "{CHECKED}: {} rows=fmt:ok base={} secs=1 extra",
                id('a'),
                id('b')
            ),
            format!("{CHECKED}: {} base={} rows=fmt:ok secs=1", id('a'), id('b')),
            format!("{COMPOSED_ONTO}: abc"),
            format!("{COMPOSED_ONTO}: {}\n{COMPOSED_ONTO}: {}", id('e'), id('f')),
            format!("{INHERITED}:  padded"),
            format!("{INHERITED}: twice\n{INHERITED}: twice"),
        ] {
            assert_eq!(
                Stated::of_trailers(&format!("{good}{bad}\n")),
                None,
                "{bad}"
            );
        }
        let sha256 = std::iter::repeat_n('f', 64).collect::<String>();
        assert!(object_id(&sha256) && !object_id(&sha256[..63]));
    }
}
