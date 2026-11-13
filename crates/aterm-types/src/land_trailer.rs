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
//! Aterm-Interrupted: <tree> base=<commit> tip=<commit> secs=<n>
//! Aterm-Rechecked: <tree> rows=<check>:<word>,… base=<commit> onto=<commit> secs=<n>
//! Aterm-Composed-Onto: <commit>
//! Aterm-Inherited: <red item id>
//! ```
//!
//! One `Aterm-Checked` line per judgment that came to a verdict, in the order they ran: the first
//! judged the branch over the cone since the main commit it merged (`base`), and each re-judge the
//! delta a later merge of main brought, since the commit judged before it. One
//! `Aterm-Interrupted` line per judgment a main tip that could not compose interrupted, in the
//! order they ran: the tree it was judging, its `base`, that `tip` (the base of the judgment after
//! it) and how long it ran; it is no verdict, and nothing reads it as one. One `Aterm-Rechecked`
//! line per recheck (`docs/DESIGN-landing-economy.md` §2.1): a passed judgment met a main tip
//! whose shared paths were only Markdown no build, include or test reads, so the tool merged
//! that tip (`onto`) and reran only the source readers (fmt, census and export-scan) on the merge
//! (`tree`), over the cone since `base`. Each follows the `Aterm-Checked` line it extends, so a
//! receipt's line order says whether its last judgment was a recheck; it is no re-judge.
//! `Aterm-Composed-Onto` names the main commit the last judgment was composed onto without a run;
//! `Aterm-Inherited` is one red item main already had outside the branch's crates per line.

/// One judgment's trailer key.
pub const CHECKED: &str = "Aterm-Checked";
/// One interrupted judgment's trailer key.
pub const INTERRUPTED: &str = "Aterm-Interrupted";
/// One recheck's trailer key.
pub const RECHECKED: &str = "Aterm-Rechecked";
/// The trailer key naming the main commit a judged tree was composed onto.
pub const COMPOSED_ONTO: &str = "Aterm-Composed-Onto";
/// The trailer key naming one red item the landing inherited from main.
pub const INHERITED: &str = "Aterm-Inherited";
/// Every key, in the order a landing writes them (for git's `%(trailers:key=…)`).
pub const KEYS: [&str; 5] = [CHECKED, INTERRUPTED, RECHECKED, COMPOSED_ONTO, INHERITED];

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

/// One judgment a main tip interrupted: the tree it was judging, the commit its cone was computed
/// from, the main tip that could not compose, and how long it ran. It is no verdict.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Interrupted {
    /// The tree the interrupted run was judging.
    pub tree: String,
    /// The commit its cone was computed from (`verify --base`).
    pub base: String,
    /// The main tip that did not compose, which the judgment after it merged.
    pub tip: String,
    /// The run's wall seconds until it ended.
    pub secs: u64,
}

/// One recheck: the merge of a passed judgment with a main tip that shared only no-reach
/// Markdown, the source readers' rows on it, the commit their cone was computed from, the tip it
/// merged, and how long they ran.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rechecked {
    /// The merge's tree object id: what the landing pushes, or what a later composition merges.
    pub tree: String,
    /// Each source reader's row, in the order the run finished them.
    pub rows: Vec<Row>,
    /// The commit the run's cone was computed from (`verify --base`).
    pub base: String,
    /// The main tip the merge composed the judgment onto.
    pub onto: String,
    /// The run's wall seconds.
    pub secs: u64,
    /// How many `Aterm-Checked` judgments ran before it: its trailer line follows the last of
    /// them. It is the line's place in the block, not a part of its value, and at most
    /// `checked.len()` in a [`Stated`].
    pub after: usize,
}

impl Rechecked {
    /// Its trailer value: `<tree> rows=<check>:<word>,… base=<commit> onto=<commit> secs=<n>`.
    #[must_use]
    pub fn value(&self) -> String {
        format!(
            "{} rows={} base={} onto={} secs={}",
            self.tree,
            rows_value(&self.rows),
            self.base,
            self.onto,
            self.secs
        )
    }

    /// The recheck a trailer `value` states, `after` judgments in; `None` unless every part has
    /// its shape.
    #[must_use]
    pub fn of_value(value: &str, after: usize) -> Option<Self> {
        let mut parts = value.split(' ');
        let tree = parts.next().filter(|id| object_id(id))?;
        let rows = of_rows(parts.next()?.strip_prefix("rows=")?)?;
        let base = parts
            .next()?
            .strip_prefix("base=")
            .filter(|id| object_id(id))?;
        let onto = parts
            .next()?
            .strip_prefix("onto=")
            .filter(|id| object_id(id))?;
        let secs = seconds(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            tree: tree.to_owned(),
            rows,
            base: base.to_owned(),
            onto: onto.to_owned(),
            secs,
            after,
        })
    }
}

impl Interrupted {
    /// Its trailer value: `<tree> base=<commit> tip=<commit> secs=<n>`.
    #[must_use]
    pub fn value(&self) -> String {
        format!(
            "{} base={} tip={} secs={}",
            self.tree, self.base, self.tip, self.secs
        )
    }

    /// The interrupted judgment a trailer `value` states; `None` unless every part has its shape.
    #[must_use]
    pub fn of_value(value: &str) -> Option<Self> {
        let mut parts = value.split(' ');
        let tree = parts.next().filter(|id| object_id(id))?;
        let base = parts
            .next()?
            .strip_prefix("base=")
            .filter(|id| object_id(id))?;
        let tip = parts
            .next()?
            .strip_prefix("tip=")
            .filter(|id| object_id(id))?;
        let secs = parts.next()?.strip_prefix("secs=")?;
        if parts.next().is_some() || !secs.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            tree: tree.to_owned(),
            base: base.to_owned(),
            tip: tip.to_owned(),
            secs: secs.parse().ok()?,
        })
    }
}

/// A landing merge's stated receipt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stated {
    /// Every judgment that came to a verdict, in the order they ran; never empty in a parsed
    /// receipt.
    pub checked: Vec<Checked>,
    /// Every judgment a main tip interrupted, in the order they ran: no verdict.
    pub interrupted: Vec<Interrupted>,
    /// Every recheck, in the order they ran: no re-judge.
    pub rechecked: Vec<Rechecked>,
    /// The main commit the last judgment was composed onto, where main moved after it.
    pub composed_onto: Option<String>,
    /// The red items the landing inherited, each named once.
    pub inherited: Vec<String>,
}

impl Checked {
    /// Its trailer value: `<tree> rows=<check>:<word>,… base=<commit> secs=<n>`.
    #[must_use]
    pub fn value(&self) -> String {
        format!(
            "{} rows={} base={} secs={}",
            self.tree,
            rows_value(&self.rows),
            self.base,
            self.secs
        )
    }

    /// The judgment a trailer `value` states; `None` unless every part has its shape.
    #[must_use]
    pub fn of_value(value: &str) -> Option<Self> {
        let mut parts = value.split(' ');
        let tree = parts.next().filter(|id| object_id(id))?;
        let rows = of_rows(parts.next()?.strip_prefix("rows=")?)?;
        let base = parts
            .next()?
            .strip_prefix("base=")
            .filter(|id| object_id(id))?;
        let secs = seconds(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            tree: tree.to_owned(),
            rows,
            base: base.to_owned(),
            secs,
        })
    }
}

impl Stated {
    /// Its trailer block, one `Key: value` line each (no blank line before it).
    #[must_use]
    pub fn trailers(&self) -> String {
        let mut block = String::new();
        let rechecks = |block: &mut String, after: &dyn Fn(usize) -> bool| {
            for recheck in self.rechecked.iter().filter(|recheck| after(recheck.after)) {
                block.push_str(&format!("{RECHECKED}: {}\n", recheck.value()));
            }
        };
        rechecks(&mut block, &|after| after == 0);
        for (ran, checked) in self.checked.iter().enumerate() {
            block.push_str(&format!("{CHECKED}: {}\n", checked.value()));
            let ran = ran + 1;
            let last = ran == self.checked.len();
            rechecks(&mut block, &|after| after == ran || (last && after > ran));
        }
        for interrupted in &self.interrupted {
            block.push_str(&format!("{INTERRUPTED}: {}\n", interrupted.value()));
        }
        if let Some(onto) = &self.composed_onto {
            block.push_str(&format!("{COMPOSED_ONTO}: {onto}\n"));
        }
        for item in &self.inherited {
            block.push_str(&format!("{INHERITED}: {item}\n"));
        }
        block
    }

    /// The judgments after the first, interrupted ones included: how often a moved main was
    /// merged and judged again. A recheck is not one.
    #[must_use]
    pub fn rejudges(&self) -> usize {
        (self.checked.len() + self.interrupted.len()).saturating_sub(1)
    }

    /// Every judgment's wall seconds, interrupted ones and rechecks included, summed.
    #[must_use]
    pub fn secs(&self) -> u64 {
        self.checked
            .iter()
            .map(|checked| checked.secs)
            .chain(self.interrupted.iter().map(|interrupted| interrupted.secs))
            .chain(self.rechecked.iter().map(|recheck| recheck.secs))
            .fold(0u64, u64::saturating_add)
    }

    /// The rechecks that extend the last full judgment (each line after its `Aterm-Checked`
    /// line), in the order they ran: empty where a judgment ran after the last recheck.
    #[must_use]
    pub fn rechecks_since_judged(&self) -> &[Rechecked] {
        let since = self
            .rechecked
            .iter()
            .rposition(|recheck| recheck.after < self.checked.len())
            .map_or(0, |at| at + 1);
        &self.rechecked[since..]
    }

    /// The main commit the landing's last judgment was composed onto: the one a run-free
    /// composition named, else the tip of the last recheck that extends the last full
    /// judgment; `None` where it landed as judged.
    #[must_use]
    pub fn onto(&self) -> Option<&str> {
        self.composed_onto.as_deref().or_else(|| {
            self.rechecks_since_judged()
                .last()
                .map(|recheck| recheck.onto.as_str())
        })
    }

    /// The receipt the trailer lines in `text` state (git's `%(trailers:…)` output, or a
    /// message's trailer block), in their order: each recheck is placed after the judgments
    /// above it. Lines with other keys are passed over. `None` where no
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
                INTERRUPTED => stated.interrupted.push(Interrupted::of_value(value)?),
                RECHECKED => stated
                    .rechecked
                    .push(Rechecked::of_value(value, stated.checked.len())?),
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

/// A judgment's `rows=` value: `<check>:<word>` per row, comma-separated.
fn rows_value(rows: &[Row]) -> String {
    rows.iter()
        .map(|row| format!("{}:{}", row.check, row.word))
        .collect::<Vec<_>>()
        .join(",")
}

/// The rows a `rows=` value states (empty for an empty value); `None` unless each is a check name
/// of lowercase letters, digits and `-` with one of [`WORDS`].
fn of_rows(rows: &str) -> Option<Vec<Row>> {
    if rows.is_empty() {
        return Some(Vec::new());
    }
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
        .collect()
}

/// The seconds a `secs=<n>` part states; `None` unless `<n>` is ASCII digits that fit.
fn seconds(part: &str) -> Option<u64> {
    let secs = part.strip_prefix("secs=")?;
    if !secs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    secs.parse().ok()
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
#[path = "land_trailer_tests.rs"]
mod tests;
