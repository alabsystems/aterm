// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stable machine identities and reliable human names. Names are assignments, not
//! identities; all prior signed assignments remain aliases in the history view.
use crate::{Error, Result, crypto::name};
use aterm_json::Value;

/// A positive decimal number of at most `max` digits with no leading zero.
fn positive(digits: &str, max: usize) -> Option<u32> {
    (!digits.is_empty()
        && digits.len() <= max
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit()))
    .then(|| digits.parse().ok())
    .flatten()
}

/// The number of `m<N>` or `M<N>`.
fn machine_number(text: &str) -> Option<u32> {
    let spelled = text.starts_with(['m', 'M']) && text.trim_ascii() == text;
    spelled
        .then(|| aterm_types::machine_name::Number::parse(text).ok())
        .flatten()
        .map(aterm_types::machine_name::Number::get)
}

/// Canonical machine name, including legacy high numbers such as m100.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(pub u32);
impl Name {
    /// Parse m0, M2, m100, etc.; ambiguous leading zeroes and more than six digits are
    /// refused (the grammar [`Label`] shares).
    pub fn parse(text: &str) -> Result<Self> {
        machine_number(text).map(Self).ok_or_else(|| {
            Error::new(
                "machine name must be m followed by a non-negative number without leading zeroes",
            )
        })
    }
    /// Whether `text` is a machine name in the grammar 0.146 and 0.147 wrote and checked: `m`
    /// or `M` and a positive number of any length that fits, where [`Self::parse`] now takes at
    /// most six digits. Only a validator of rows those builds replicated reads it.
    pub fn legacy(text: &str) -> bool {
        text.strip_prefix(['m', 'M']).is_some_and(legacy_number)
    }
    /// Lowercase stable display form.
    pub fn canonical(&self) -> String {
        format!("m{}", self.0)
    }
}

/// A positive decimal number with no leading zero that fits a `u32`, the 0.146/0.147 grammar.
fn legacy_number(digits: &str) -> bool {
    !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits.parse::<u32>().is_ok()
}

/// A machine label as the lab stores it (docs/DESIGN-alab-lab-model-2026-11-01.md §3.3): `m`
/// and a non-negative number with no leading zero (`m100` and up included), with the membership
/// log's clash suffix `-xxxx` (four hex digits) where two Macs claimed one number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label {
    pub number: u32,
    /// The clash suffix's four hex digits.
    pub clash: Option<u16>,
}

impl Label {
    /// A plain `m<N>`.
    pub const fn plain(number: u32) -> Self {
        Self {
            number,
            clash: None,
        }
    }

    /// Parse one machine label typed any way the sources spell it (`m40`, `M109`,
    /// `m40-3FA2`); a slot or a two-Mac label is refused here ([`Reading::parse`] reads those).
    ///
    /// # Errors
    /// It is not a machine label.
    pub fn parse(text: &str) -> Result<Self> {
        match Reading::parse(text)? {
            Reading::Machine(label) => Ok(label),
            _ => Err(Error::new("a machine label names one Mac, not a slot")),
        }
    }

    /// Parse a label as stored: already lower-case and trimmed, so a stored cell has one
    /// spelling.
    ///
    /// # Errors
    /// It is not a stored machine label.
    pub fn parse_stored(text: &str) -> Result<Self> {
        let label = Self::parse(text)?;
        if text.trim_ascii().len() == text.len() && !text.bytes().any(|b| b.is_ascii_uppercase()) {
            Ok(label)
        } else {
            Err(Error::new(
                "a stored machine label is lower-case and trimmed",
            ))
        }
    }

    /// The stored spelling: `m40` or `m40-3fa2`.
    pub fn canonical(&self) -> String {
        match self.clash {
            Some(clash) => format!("m{}-{clash:04x}", self.number),
            None => format!("m{}", self.number),
        }
    }

    /// The machine number as a [`Name`], without any clash suffix.
    pub const fn name(&self) -> Name {
        Name(self.number)
    }
}

/// One preference a label names (design §0, §4.15): a machine, and the order the label states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferred {
    pub machine: Label,
    /// The slot number, an order and nothing else; a two-Mac label states none.
    pub rank: Option<u32>,
}

/// What one label cell names: **the one parser** for every label spelling the lab's sources use
/// (design §9), which `Name`, `Slot` and stored `label` cells all go through.
///
/// - `m<N>` or `M<N>`: a machine, and as a preference its rank 1;
/// - `m<N>-xxxx`, four hex digits: a machine whose number the membership log suffixed on a
///   clash, never a slot;
/// - `m<N>-<k>`, one to three digits: the legacy spelling of number k (`m27-2`);
/// - `m<N>.<k>`: number k on that machine, which a preference reads as rank k;
/// - `m<N>-m<M>`: a harness account label spanning two Macs (`m106-m38`), one preference on each.
///
/// Surrounding ASCII space is ignored and the result is lower-case. Labels fold through the
/// roster's aliases (m100 and m32) only where the roster says so, which is the importer's
/// lookup, never this grammar's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Machine(Label),
    Slot(Label, u32),
    Pair { first: Label, second: Label },
}

/// The most digits a slot number has, in either spelling.
const MAX_SLOT_DIGITS: usize = 3;

impl Reading {
    /// Read one label cell.
    ///
    /// # Errors
    /// It is none of the forms above. The refusal never quotes the cell.
    pub fn parse(text: &str) -> Result<Self> {
        let refused = || Error::new("not a machine label, slot or two-Mac label");
        let text = text.trim_ascii();
        if let Some((machine, slot)) = text.split_once('.') {
            let number = machine_number(machine).ok_or_else(refused)?;
            let slot = positive(slot, MAX_SLOT_DIGITS).ok_or_else(refused)?;
            return Ok(Self::Slot(Label::plain(number), slot));
        }
        let Some((machine, rest)) = text.split_once('-') else {
            return machine_number(text)
                .map(|number| Self::Machine(Label::plain(number)))
                .ok_or_else(refused);
        };
        let number = machine_number(machine).ok_or_else(refused)?;
        if let Some(second) = machine_number(rest) {
            if second == number {
                return Err(refused());
            }
            return Ok(Self::Pair {
                first: Label::plain(number),
                second: Label::plain(second),
            });
        }
        if rest.len() == 4 && rest.bytes().all(|b| b.is_ascii_hexdigit()) {
            let clash = u16::from_str_radix(rest, 16).map_err(|_| refused())?;
            return Ok(Self::Machine(Label {
                number,
                clash: Some(clash),
            }));
        }
        positive(rest, MAX_SLOT_DIGITS)
            .map(|slot| Self::Slot(Label::plain(number), slot))
            .ok_or_else(refused)
    }

    /// The machine it names first: the machine, the slot's machine, or a pair's first.
    pub const fn machine(&self) -> &Label {
        match self {
            Self::Machine(label) | Self::Slot(label, _) | Self::Pair { first: label, .. } => label,
        }
    }

    /// The preferences it names, in order: a bare machine is rank 1, as the sheets' `m3`, `m3.2`
    /// order their accounts; a number is that rank; a two-Mac label is one unranked preference
    /// on each Mac. The grammar never says what an account is, only where it is preferred.
    pub fn preferences(&self) -> Vec<Preferred> {
        match *self {
            Self::Machine(machine) => vec![Preferred {
                machine,
                rank: Some(1),
            }],
            Self::Slot(machine, rank) => vec![Preferred {
                machine,
                rank: Some(rank),
            }],
            Self::Pair { first, second } => vec![
                Preferred {
                    machine: first,
                    rank: None,
                },
                Preferred {
                    machine: second,
                    rank: None,
                },
            ],
        }
    }
}

/// Display slot m3.2; resolved through the registry, never through hostname guessing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    /// Name when this label was assigned.
    pub machine: Name,
    /// One-based account number.
    pub number: u32,
}
impl Slot {
    /// Dot notation or historical dash notation (one to three digits); a bare m3 is its first
    /// slot. Read through [`Reading::parse`], so a clash-suffixed machine (`m40-3fa2`) or a
    /// two-Mac label is not a slot.
    pub fn parse(text: &str) -> Result<Self> {
        match Reading::parse(text)? {
            Reading::Machine(Label {
                number,
                clash: None,
            }) => Ok(Self {
                machine: Name(number),
                number: 1,
            }),
            Reading::Slot(label, number) => Ok(Self {
                machine: label.name(),
                number,
            }),
            _ => Err(Error::new("account slot must be a positive number")),
        }
    }
    /// Whether `text` is a slot in the grammar 0.146 and 0.147 wrote and checked: `m<N>`, then
    /// optionally `.` or `-` and a positive number, any length that fits. Only a validator of
    /// rows those builds replicated reads it (`accounts/imports`' `machine_slot_hint`), so a row
    /// such as `m40-1234`, now a clash-suffixed machine and not a slot, still validates and its
    /// writer's log is not refused; every new reading goes through [`Self::parse`].
    pub fn legacy(text: &str) -> bool {
        let (machine, number) = text
            .split_once(['.', '-'])
            .map_or((text, "1"), |(m, n)| (m, n));
        Name::legacy(machine) && legacy_number(number)
    }
    /// Canonical account slot.
    pub fn canonical(&self) -> String {
        format!("m{}.{}", self.machine.0, self.number)
    }
}
/// Validate a stable-id keyed assignment; ingestion separately enforces authority.
pub fn validate(key: &str, value: &Value) -> Result<()> {
    crate::database::decode_hex::<16>(key)?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("machine assignment must be an object"))?;
    if object
        .keys()
        .any(|key| !["name", "legacy_names", "hostnames", "details"].contains(&key.as_str()))
    {
        return Err(Error::new("unknown machine assignment field"));
    }
    let assignment_name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("machine needs a canonical name"))?;
    // Current assignments include zero; retain the larger numbers older builds wrote.
    if Name::parse(assignment_name).is_err() && !Name::legacy(assignment_name) {
        return Err(Error::new(
            "machine name must be m followed by a non-negative number without leading zeroes",
        ));
    }
    for field in ["legacy_names", "hostnames"] {
        if let Some(list) = value.get(field) {
            let list = list
                .as_array()
                .filter(|l| l.len() <= 64)
                .ok_or_else(|| Error::new("machine names must be a bounded list"))?;
            if !list.iter().all(|v| v.as_str().is_some_and(name)) {
                return Err(Error::new("invalid machine alias or hostname"));
            }
        }
    }
    if let Some(details) = value.get("details") {
        crate::contacts::validate_details(details)?;
    }
    Ok(())
}
/// All names this signed assignment contributes to the monotone history index.
pub fn names(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(text) = value.get("name").and_then(Value::as_str)
        && let Ok(n) = Name::parse(text)
    {
        names.push(n.canonical());
    }
    if let Some(legacy) = value.get("legacy_names").and_then(Value::as_array) {
        for n in legacy.iter().filter_map(Value::as_str) {
            names.push(Name::parse(n).map_or_else(|_| n.to_owned(), |n| n.canonical()));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_names_and_account_slots_keep_their_meaning() {
        assert_eq!(Name::parse("M100").unwrap().canonical(), "m100");
        assert_eq!(Name::parse("m0").unwrap().canonical(), "m0");
        assert_eq!(Slot::parse("m3.2").unwrap().number, 2);
        assert_eq!(Slot::parse("m27-2").unwrap().canonical(), "m27.2");
        assert_eq!(Slot::parse("m3").unwrap().number, 1);
        for bad in ["m01", "m-1", "m3.2"] {
            assert!(Name::parse(bad).is_err());
        }
    }

    fn label(number: u32) -> Label {
        Label::plain(number)
    }

    /// The one label parser reads every spelling the lab's sources use the same way (design §9):
    /// `M109` folds to `m109`; four hex digits after a dash are the membership log's clash
    /// suffix, one to three digits the legacy number; a dot is a number; `m106-m38` is one
    /// unranked preference on m106 and one on m38; and `Name`, `Slot` and stored label cells agree with it.
    /// NEGATIVE CONTROL: with the clash arm removed from `Reading::parse` (four hex digits read
    /// as a legacy slot, which has at most three), `m40-3fa2` and `m40-1234` are refused, and
    /// this fails at its `m40-3fa2` row: `` called `Result::unwrap()` on an `Err` value ``.
    #[test]
    fn one_parser_reads_every_label_spelling() {
        let clash = |number, clash| Label {
            number,
            clash: Some(clash),
        };
        for (text, reading) in [
            ("m40", Reading::Machine(label(40))),
            (" M109 ", Reading::Machine(label(109))),
            ("m112", Reading::Machine(label(112))),
            ("m40-3fa2", Reading::Machine(clash(40, 0x3fa2))),
            ("M40-3FA2", Reading::Machine(clash(40, 0x3fa2))),
            ("m40-1234", Reading::Machine(clash(40, 0x1234))),
            ("m27-2", Reading::Slot(label(27), 2)),
            ("m27-100", Reading::Slot(label(27), 100)),
            ("m3.2", Reading::Slot(label(3), 2)),
            (
                "m106-m38",
                Reading::Pair {
                    first: label(106),
                    second: label(38),
                },
            ),
        ] {
            assert_eq!(Reading::parse(text).unwrap(), reading, "{text}");
        }
        for bad in [
            "",
            "m",
            "m01",
            "x1",
            "m1234567",
            "m3.0",
            "m3.02",
            "m3.1000",
            "m27-02",
            "m27-2a",
            "m40-3fa",
            "m40-3fa2b",
            "m106-m106",
            "m106-m38-m2",
            "m3.2.1",
            "mac-mini-m4",
        ] {
            assert!(Reading::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            Reading::parse("m106-m38").unwrap().preferences(),
            [
                Preferred {
                    machine: label(106),
                    rank: None,
                },
                Preferred {
                    machine: label(38),
                    rank: None,
                },
            ]
        );
        assert_eq!(Reading::parse("m3").unwrap().preferences()[0].rank, Some(1));
        assert_eq!(
            Reading::parse("m27-3").unwrap().preferences()[0].rank,
            Some(3)
        );
        assert_eq!(Reading::parse("m27-2").unwrap().machine(), &label(27));
        // Stored cells have one spelling; Name and Slot read through the same grammar.
        assert_eq!(Label::parse("M40-3FA2").unwrap().canonical(), "m40-3fa2");
        assert!(Label::parse_stored("m40-3fa2").is_ok());
        for unstored in ["M40", " m40", "m40-3FA2", "m27-2", "m106-m38"] {
            assert!(Label::parse_stored(unstored).is_err(), "{unstored}");
        }
        assert!(Slot::parse("m40-3fa2").is_err());
        assert!(Slot::parse("m40-1234").is_err());
        assert!(Slot::parse("m106-m38").is_err());
        assert!(Name::parse("m1234567").is_err());
    }

    /// A machine assignment 0.146 or 0.147 replicated keeps validating: those builds took any
    /// machine number that fits, where `Name::parse` now takes six digits; a name no build
    /// wrote is still refused.
    /// NEGATIVE CONTROL: with `validate` reading the name through `Name::parse`, the
    /// seven-digit name is refused: `` called `Result::unwrap()` on an `Err` value ``.
    #[test]
    fn assignments_older_builds_replicated_keep_validating() {
        let key = "cd".repeat(16);
        validate(&key, &aterm_json::json!({"name": "m1234567"})).unwrap();
        validate(&key, &aterm_json::json!({"name": "M3"})).unwrap();
        validate(&key, &aterm_json::json!({"name": "m0"})).unwrap();
        for bad in ["m01", "m", "x3", "m3-2"] {
            assert!(
                validate(&key, &aterm_json::json!({"name": bad})).is_err(),
                "{bad}"
            );
        }
    }
}
