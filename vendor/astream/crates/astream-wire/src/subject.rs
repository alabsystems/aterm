//! The astream address grammar.
//!
//! Two distinct newtypes, validated in their constructors:
//!
//! * [`Subject`] — a publish target. No wildcards. e.g. `/a/stream/events`.
//! * [`Filter`] — a subscription pattern. NATS-style segment wildcards:
//!   `*` matches exactly one segment, `>` matches one-or-more trailing
//!   segments and must be the last segment. Partial-segment wildcards
//!   (`fo*`) are rejected.
//!
//! There is intentionally **no** public unchecked constructor on the publish
//! path (kafka2 exposed `new_unchecked`). Both types require a leading `/` and
//! non-empty segments.

use std::fmt;

/// Iterate the segments of a `/`-rooted path, dropping the leading empty piece.
fn segments(path: &str) -> impl Iterator<Item = &str> {
    // `path` starts with '/', so `split('/')` yields "" first; skip it.
    path.split('/').skip(1)
}

// ---------------------------------------------------------------------------
// Subject (publish target)
// ---------------------------------------------------------------------------

/// Why a string is not a valid [`Subject`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectError {
    Empty,
    MissingLeadingSlash,
    EmptySegment,
    ContainsWildcard,
    /// A segment contains a control byte (NUL, newline, tab, 0x1f, DEL, ...).
    ControlByte,
}

impl fmt::Display for SubjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            SubjectError::Empty => "subject is empty",
            SubjectError::MissingLeadingSlash => "subject must start with '/'",
            SubjectError::EmptySegment => "subject has an empty segment",
            SubjectError::ContainsWildcard => "subject must not contain '*' or '>'",
            SubjectError::ControlByte => "subject must not contain control bytes",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for SubjectError {}

/// A validated publish target. Guaranteed: starts with `/`, every segment is
/// non-empty, and no segment contains a wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subject(String);

impl Subject {
    /// Validate and construct, or explain why the input is rejected.
    pub fn new(s: impl Into<String>) -> Result<Self, SubjectError> {
        let s = s.into();
        if s.is_empty() {
            return Err(SubjectError::Empty);
        }
        if !s.starts_with('/') {
            return Err(SubjectError::MissingLeadingSlash);
        }
        for seg in segments(&s) {
            if seg.is_empty() {
                return Err(SubjectError::EmptySegment);
            }
            if seg.contains('*') || seg.contains('>') {
                return Err(SubjectError::ContainsWildcard);
            }
            if seg.bytes().any(|b| b < 0x20 || b == 0x7f) {
                return Err(SubjectError::ControlByte);
            }
        }
        Ok(Subject(s))
    }

    /// The full subject path.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The segments, wildcard-free by construction.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        segments(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Filter (subscription pattern)
// ---------------------------------------------------------------------------

/// Why a string is not a valid [`Filter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterError {
    Empty,
    MissingLeadingSlash,
    EmptySegment,
    /// A wildcard char appears inside an otherwise-literal segment (`fo*`).
    PartialWildcard,
    /// `>` appeared somewhere other than the final segment.
    MultiNotLast,
    /// A segment contains a control byte (NUL, newline, tab, 0x1f, DEL, ...).
    ControlByte,
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            FilterError::Empty => "filter is empty",
            FilterError::MissingLeadingSlash => "filter must start with '/'",
            FilterError::EmptySegment => "filter has an empty segment",
            FilterError::PartialWildcard => "wildcards must be a whole segment ('*' or '>')",
            FilterError::MultiNotLast => "'>' must be the final segment",
            FilterError::ControlByte => "filter must not contain control bytes",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for FilterError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Literal(String),
    /// `*` — exactly one segment.
    Single,
    /// `>` — one or more trailing segments.
    Multi,
}

/// A validated subscription pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    raw: String,
    segs: Vec<Seg>,
}

impl Filter {
    /// Validate and construct, or explain why the input is rejected.
    pub fn new(s: impl Into<String>) -> Result<Self, FilterError> {
        let s = s.into();
        if s.is_empty() {
            return Err(FilterError::Empty);
        }
        if !s.starts_with('/') {
            return Err(FilterError::MissingLeadingSlash);
        }
        let parts: Vec<&str> = segments(&s).collect();
        let mut segs = Vec::with_capacity(parts.len());
        let last = parts.len().saturating_sub(1);
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                return Err(FilterError::EmptySegment);
            }
            match *part {
                "*" => segs.push(Seg::Single),
                ">" => {
                    if i != last {
                        return Err(FilterError::MultiNotLast);
                    }
                    segs.push(Seg::Multi);
                }
                lit => {
                    if lit.contains('*') || lit.contains('>') {
                        return Err(FilterError::PartialWildcard);
                    }
                    if lit.bytes().any(|b| b < 0x20 || b == 0x7f) {
                        return Err(FilterError::ControlByte);
                    }
                    segs.push(Seg::Literal(lit.to_string()));
                }
            }
        }
        Ok(Filter { raw: s, segs })
    }

    /// The original pattern string.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Does this filter match the given subject?
    ///
    /// Iterative matcher. (`tests/router_differential.rs` checks it against an
    /// independently-coded recursive oracle over generated well-formed filters and
    /// subjects; the malformed and edge inputs go to the *validators*
    /// [`Filter::new`]/[`Subject::new`], whose every error variant that test
    /// reaches by construction.)
    pub fn matches(&self, subject: &Subject) -> bool {
        let subj: Vec<&str> = subject.segments().collect();
        let mut i = 0usize;
        loop {
            match self.segs.get(i) {
                // `>` matches one or more remaining segments: require at least one.
                Some(Seg::Multi) => return subj.len() > i,
                Some(Seg::Single) => {
                    if i >= subj.len() {
                        return false;
                    }
                }
                Some(Seg::Literal(lit)) => match subj.get(i) {
                    Some(seg) if seg == lit => {}
                    _ => return false,
                },
                // Filter exhausted: match iff subject is exhausted too.
                None => return i == subj.len(),
            }
            i += 1;
        }
    }

    /// Whether this filter **contains** `other`: every subject matching `other`
    /// also matches `self`. This is the capability-scoping primitive — a bearer
    /// granted `self` may subscribe with any `other` this contains. It is
    /// **sound** (never a false positive): where containment is uncertain it
    /// returns `false`, so an ACL built on it can only be too strict, never too
    /// permissive. `>` (1+ trailing) contains any longer or equal non-empty tail;
    /// `*` (exactly one) contains a single literal/`*` but never `>`; a literal
    /// contains only the identical literal.
    #[must_use]
    pub fn contains(&self, other: &Filter) -> bool {
        let (a, b) = (&self.segs, &other.segs);
        let mut i = 0usize;
        loop {
            match a.get(i) {
                // `>` accepts every subject with >= i+1 segments; every subject
                // matching `b` has that iff `b` still has a segment at i.
                Some(Seg::Multi) => return b.len() > i,
                Some(Seg::Single) => match b.get(i) {
                    // `b` could match a longer, variable tail here — not contained.
                    None | Some(Seg::Multi) => return false,
                    Some(Seg::Single | Seg::Literal(_)) => {}
                },
                Some(Seg::Literal(la)) => match b.get(i) {
                    Some(Seg::Literal(lb)) if lb == la => {}
                    _ => return false,
                },
                // `a` exhausted (no trailing `>`): contained iff `b` ends here too.
                None => return b.len() == i,
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_validation() {
        assert!(Subject::new("/a/stream/events").is_ok());
        assert_eq!(Subject::new(""), Err(SubjectError::Empty));
        assert_eq!(Subject::new("a/b"), Err(SubjectError::MissingLeadingSlash));
        assert_eq!(Subject::new("/a//b"), Err(SubjectError::EmptySegment));
        assert_eq!(Subject::new("/a/"), Err(SubjectError::EmptySegment));
        assert_eq!(Subject::new("/a/*"), Err(SubjectError::ContainsWildcard));
        assert_eq!(Subject::new("/a/>"), Err(SubjectError::ContainsWildcard));
    }

    #[test]
    fn filter_validation_rejects_partial_and_misplaced_wildcards() {
        assert!(Filter::new("/a/*/c").is_ok());
        assert!(Filter::new("/a/>").is_ok());
        assert_eq!(Filter::new("/a/fo*"), Err(FilterError::PartialWildcard));
        assert_eq!(Filter::new("/a/>/c"), Err(FilterError::MultiNotLast));
        assert_eq!(Filter::new("/a//c"), Err(FilterError::EmptySegment));
        assert_eq!(Filter::new("a/b"), Err(FilterError::MissingLeadingSlash));
    }

    #[test]
    fn matching_basics() {
        let m = |f: &str, s: &str| Filter::new(f).unwrap().matches(&Subject::new(s).unwrap());
        assert!(m("/a/stream/events", "/a/stream/events"));
        assert!(!m("/a/stream/events", "/a/stream/other"));
        assert!(m("/a/*/events", "/a/stream/events"));
        assert!(!m("/a/*/events", "/a/stream/sub/events"));
        assert!(m("/a/>", "/a/stream/events"));
        assert!(m("/a/>", "/a/x"));
    }

    #[test]
    fn multi_requires_at_least_one_segment() {
        // The locked edge case: `/a/>` does NOT match `/a` alone.
        let f = Filter::new("/a/>").unwrap();
        assert!(!f.matches(&Subject::new("/a").unwrap()));
        assert!(f.matches(&Subject::new("/a/b").unwrap()));
    }

    #[test]
    fn subject_rejects_control_bytes() {
        assert!(Subject::new("/a/b").is_ok());
        assert_eq!(Subject::new("/a/\u{0}b"), Err(SubjectError::ControlByte));
        assert_eq!(Subject::new("/a/\u{1f}b"), Err(SubjectError::ControlByte));
        assert_eq!(Subject::new("/a/\u{7f}b"), Err(SubjectError::ControlByte));
        // structural errors still take precedence over the control-byte check
        assert_eq!(Subject::new("/a//\u{0}"), Err(SubjectError::EmptySegment));
    }

    #[test]
    fn filter_rejects_control_bytes() {
        assert!(Filter::new("/a/*/c").is_ok());
        assert_eq!(Filter::new("/a/\nb"), Err(FilterError::ControlByte));
        assert_eq!(Filter::new("/a/\u{1f}b"), Err(FilterError::ControlByte));
        // wildcard-structure errors keep precedence over the control-byte check
        assert_eq!(Filter::new("/a/fo*"), Err(FilterError::PartialWildcard));
    }

    #[test]
    fn single_requires_exactly_one_segment() {
        let f = Filter::new("/a/*").unwrap();
        assert!(f.matches(&Subject::new("/a/b").unwrap()));
        assert!(!f.matches(&Subject::new("/a").unwrap()));
        assert!(!f.matches(&Subject::new("/a/b/c").unwrap()));
    }
}
