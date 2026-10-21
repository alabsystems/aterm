// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `sys` JSON (§13.4): ONE SHAPE PER FIGURE, so a parser can always tell a
//! bound from a range from an unknown.
//!
//! * an exact known figure is a bare integer: `"cpu_mc": 9760`;
//! * a bounded figure is `{"v": n, "bound": "at_least" | "at_most", "cause":
//!   <why>}`, with `"unread": k` exactly when the cause is `refused`;
//! * an unknown figure is `{"why": "<word>"}`, never `null` and never 0;
//! * a range is `{"lo": n, "hi": n}`.
//!
//! [`Jv`] has no float and no null variant, so neither can be written; the
//! constructors below are the only way the other three shapes are made, and
//! [`contract_violations`] walks a reply for anything else.

/// A JSON value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Jv {
    /// An integer.
    Int(i64),
    /// A string.
    Str(String),
    /// A boolean.
    Bool(bool),
    /// An array.
    Arr(Vec<Jv>),
    /// An object, in insertion order.
    Obj(Vec<(String, Jv)>),
}

impl Jv {
    /// An object from `(key, value)` pairs.
    #[must_use]
    pub fn obj<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Jv)>) -> Self {
        Self::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// A string.
    #[must_use]
    pub fn s(text: impl Into<String>) -> Self {
        Self::Str(text.into())
    }

    /// An exact integer from any unsigned figure (saturating at `i64::MAX`).
    #[must_use]
    pub fn u(v: u64) -> Self {
        Self::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }

    /// An unknown figure: `{"why": word}`.
    #[must_use]
    pub fn why(word: &str) -> Self {
        Self::obj([("why", Self::s(word))])
    }

    /// A lower bound.
    #[must_use]
    pub fn at_least(v: u64, cause: &str) -> Self {
        Self::bound(v, "at_least", cause, None)
    }

    /// An upper bound.
    #[must_use]
    pub fn at_most(v: u64, cause: &str) -> Self {
        Self::bound(v, "at_most", cause, None)
    }

    /// A lower bound because the OS refused `unread` members.
    #[must_use]
    pub fn at_least_refused(v: u64, unread: u32) -> Self {
        Self::bound(v, "at_least", "refused", Some(unread))
    }

    fn bound(v: u64, bound: &str, cause: &str, unread: Option<u32>) -> Self {
        let mut pairs = vec![
            ("v".to_owned(), Self::u(v)),
            ("bound".to_owned(), Self::s(bound)),
            ("cause".to_owned(), Self::s(cause)),
        ];
        if let Some(n) = unread {
            pairs.push(("unread".to_owned(), Self::Int(i64::from(n))));
        }
        Self::Obj(pairs)
    }

    /// A range.
    #[must_use]
    pub fn range(lo: u64, hi: u64) -> Self {
        Self::obj([("lo", Self::u(lo.min(hi))), ("hi", Self::u(hi.max(lo)))])
    }

    /// A known figure or an unknown one.
    #[must_use]
    pub fn known_or(v: Option<u64>, why: &str) -> Self {
        v.map_or_else(|| Self::why(why), Self::u)
    }

    /// The value at `key` of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Jv> {
        match self {
            Self::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Set `key` on an object (replacing an existing value).
    pub fn set(&mut self, key: &str, value: Jv) {
        if let Self::Obj(pairs) = self {
            if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value;
            } else {
                pairs.push((key.to_owned(), value));
            }
        }
    }

    /// The compact text.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Int(n) => out.push_str(&n.to_string()),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Str(s) => write_str(out, s),
            Self::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Self::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The keys a split object holds, and nothing else (§13.4).
pub const SPLIT_PARTS: [&str; 5] = ["tab", "aterm_rest", "others", "services", "unaccounted"];

/// Every way `v` breaks the shape contract, as `path: what` lines; empty when
/// it holds. The rules: an object with `v` is exactly a bound; with `why`,
/// exactly an unknown whose word is in the closed set; with `lo`, exactly a
/// range; a key ending `_split` or named `split_mc`/`split_pm`, when an object
/// of parts, holds exactly [`SPLIT_PARTS`].
#[must_use]
pub fn contract_violations(v: &Jv) -> Vec<String> {
    let mut out = Vec::new();
    walk(v, "$", &mut out);
    out
}

fn walk(v: &Jv, path: &str, out: &mut Vec<String>) {
    match v {
        Jv::Arr(items) => {
            for (i, item) in items.iter().enumerate() {
                walk(item, &format!("{path}[{i}]"), out);
            }
        }
        Jv::Obj(pairs) => {
            let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
            let has = |k: &str| keys.contains(&k);
            if has("v") {
                check_bound(v, path, out);
                return;
            }
            if has("why") {
                let ok = keys.len() == 1
                    && matches!(v.get("why"), Some(Jv::Str(w)) if super::is_unavailable_word(w));
                if !ok {
                    out.push(format!("{path}: an unknown is exactly {{\"why\": <word>}}"));
                }
                return;
            }
            if has("lo") || has("hi") {
                let ok = keys.len() == 2
                    && matches!((v.get("lo"), v.get("hi")), (Some(Jv::Int(lo)), Some(Jv::Int(hi))) if lo <= hi);
                if !ok {
                    out.push(format!(
                        "{path}: a range is exactly {{\"lo\": n, \"hi\": n}}, lo <= hi"
                    ));
                }
                return;
            }
            for (k, child) in pairs {
                let child_path = format!("{path}.{k}");
                if (k == "split_mc" || k == "split_pm" || k.ends_with("_split"))
                    && matches!(child, Jv::Obj(_))
                    && child.get("why").is_none()
                {
                    let mut got: Vec<&str> = match child {
                        Jv::Obj(p) => p.iter().map(|(k, _)| k.as_str()).collect(),
                        _ => Vec::new(),
                    };
                    got.sort_unstable();
                    let mut want = SPLIT_PARTS.to_vec();
                    want.sort_unstable();
                    if got != want {
                        out.push(format!(
                            "{child_path}: a split holds exactly {SPLIT_PARTS:?}"
                        ));
                    }
                }
                walk(child, &child_path, out);
            }
        }
        Jv::Int(_) | Jv::Str(_) | Jv::Bool(_) => {}
    }
}

fn check_bound(v: &Jv, path: &str, out: &mut Vec<String>) {
    let Jv::Obj(pairs) = v else {
        return;
    };
    let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    let refused = matches!(v.get("cause"), Some(Jv::Str(c)) if c == "refused");
    let want = if refused { 4 } else { 3 };
    let ok = keys.len() == want
        && matches!(v.get("v"), Some(Jv::Int(n)) if *n >= 0)
        && matches!(v.get("bound"), Some(Jv::Str(b)) if b == "at_least" || b == "at_most")
        && matches!(v.get("cause"), Some(Jv::Str(c)) if !c.is_empty())
        && (!refused || matches!(v.get("unread"), Some(Jv::Int(n)) if *n >= 0));
    if !ok {
        out.push(format!(
            "{path}: a bound is exactly {{\"v\", \"bound\", \"cause\"}} (+ \"unread\" when refused)"
        ));
    }
}

/// The value of a split part in millicores (or any unit) when it is a known
/// figure or a bound; `None` for an unknown.
#[must_use]
pub fn part_value(v: &Jv) -> Option<i64> {
    match v {
        Jv::Int(n) => Some(*n),
        Jv::Obj(_) => match v.get("v") {
            Some(Jv::Int(n)) => Some(*n),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_shapes_write_as_the_design_spells_them() {
        assert_eq!(Jv::Int(9760).to_text(), "9760");
        assert_eq!(
            Jv::at_least_refused(64, 326).to_text(),
            r#"{"v":64,"bound":"at_least","cause":"refused","unread":326}"#
        );
        assert_eq!(
            Jv::at_most(2540, "reaped_no_tier").to_text(),
            r#"{"v":2540,"bound":"at_most","cause":"reaped_no_tier"}"#
        );
        assert_eq!(
            Jv::why("lane_off:ioreport").to_text(),
            r#"{"why":"lane_off:ioreport"}"#
        );
        assert_eq!(Jv::range(1932, 1825).to_text(), r#"{"lo":1825,"hi":1932}"#);
        assert_eq!(Jv::s("a\"b\\c\n\u{1}").to_text(), r#""a\"b\\c\n\u0001""#);
    }

    #[test]
    fn the_contract_walk_catches_every_other_shape() {
        let good = Jv::obj([
            ("cpu_mc", Jv::Int(10)),
            ("runnable", Jv::at_least_refused(1, 2)),
            ("mhz", Jv::range(1, 2)),
            ("gpu", Jv::why("unsupported")),
            ("split_mc", Jv::obj(SPLIT_PARTS.map(|k| (k, Jv::Int(1))))),
        ]);
        assert!(
            contract_violations(&good).is_empty(),
            "{:?}",
            contract_violations(&good)
        );
        let bad = [
            Jv::obj([("x", Jv::obj([("why", Jv::s("zero"))]))]),
            Jv::obj([("x", Jv::obj([("why", Jv::s("refused")), ("v", Jv::Int(1))]))]),
            Jv::obj([("x", Jv::obj([("v", Jv::Int(1)), ("bound", Jv::s("about"))]))]),
            Jv::obj([(
                "x",
                Jv::obj([
                    ("v", Jv::Int(1)),
                    ("bound", Jv::s("at_least")),
                    ("cause", Jv::s("refused")),
                ]),
            )]),
            Jv::obj([("x", Jv::obj([("lo", Jv::Int(3)), ("hi", Jv::Int(2))]))]),
            Jv::obj([(
                "split_mc",
                Jv::obj([("tab", Jv::Int(1)), ("aterm", Jv::Int(2))]),
            )]),
        ];
        for b in bad {
            assert!(!contract_violations(&b).is_empty(), "{}", b.to_text());
        }
    }
}
