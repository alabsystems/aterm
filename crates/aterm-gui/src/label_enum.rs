// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `label_enum!`.

/// Declare a label table.
macro_rules! label_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $label:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        $vis enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        #[allow(dead_code, reason = "one accessor set for every label table; no table calls all of it")]
        impl $name {
            /// Every variant, in index order.
            pub(crate) const ALL: [Self; Self::COUNT] = [$(Self::$variant),+];
            /// How many variants: the width of a per-variant table.
            pub(crate) const COUNT: usize = [$($label),+].len();

            /// The dense index, `0..COUNT`: the discriminant itself.
            #[must_use]
            pub(crate) const fn index(self) -> usize {
                self as usize
            }

            /// The variant at `index`; `None` past the last.
            #[must_use]
            pub(crate) const fn from_index(index: usize) -> Option<Self> {
                if index < Self::COUNT {
                    Some(Self::ALL[index])
                } else {
                    None
                }
            }

            /// The wire label.
            #[must_use]
            pub(crate) const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }

            /// The variant's identifier, symbol-free in a stripped build.
            #[must_use]
            pub(crate) const fn ident(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant)),+
                }
            }
        }

        const _: () = {
            let mut i = 0;
            while i < $name::COUNT {
                assert!($name::ALL[i].index() == i, "ALL is not in index order");
                i += 1;
            }
            assert!(
                $crate::label_enum::labels_distinct(&[$($label),+]),
                concat!(stringify!($name), ": two variants share a label"),
            );
            assert!(
                $crate::label_enum::labels_wire_safe(&[$($label),+]),
                concat!(stringify!($name), ": a label is empty or not [a-z0-9_]"),
            );
        };
    };
}

pub(crate) use label_enum;

/// Whether no two of `labels` are equal.
#[must_use]
pub(crate) const fn labels_distinct(labels: &[&str]) -> bool {
    let mut i = 0;
    while i < labels.len() {
        let mut j = i + 1;
        while j < labels.len() {
            if bytes_equal(labels[i].as_bytes(), labels[j].as_bytes()) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// Whether every label is non-empty and spelled in `[a-z0-9_]` only.
#[must_use]
pub(crate) const fn labels_wire_safe(labels: &[&str]) -> bool {
    let mut i = 0;
    while i < labels.len() {
        let bytes = labels[i].as_bytes();
        if bytes.is_empty() {
            return false;
        }
        let mut k = 0;
        while k < bytes.len() {
            if !matches!(bytes[k], b'a'..=b'z' | b'0'..=b'9' | b'_') {
                return false;
            }
            k += 1;
        }
        i += 1;
    }
    true
}

const fn bytes_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut k = 0;
    while k < a.len() {
        if a[k] != b[k] {
            return false;
        }
        k += 1;
    }
    true
}

#[cfg(test)]
#[path = "label_enum_tests.rs"]
mod tests;
