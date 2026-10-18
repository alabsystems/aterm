//! Canonical fabric receipt and deadline grammar (`REQUIREMENTS` R8, §4.2 `ack`,
//! §6.4 `dl=` / `expired`).
//!
//! Two explicitly formatted receipt bodies travel on `/f/<F>/in/...` subjects so
//! independent bridges (`aterm-link`, `asb`, `aspump`) never disagree on receipt or
//! deadline-expiry wire representations:
//!
//! ```text
//! ack re=<off> verdict=handled|refused|deferred bt=<ms>
//! expired re=<off> dl=<ms> bt=<ms>
//! ```
//!
//! - `ack`: emitted by the recipient's bridge when a message at log offset `<off>`
//!   is acted upon (`handled`, `refused`, or `deferred`) at broker timestamp `bt=<ms>`.
//! - `expired`: emitted by the ASKER's own bridge when its deadline `dl=<ms>` passes
//!   at `bt=<ms>` (`bt >= dl`) with no `answer`, `report`, or `ack` referencing
//!   `re=<off>`. The broker holds no timers; both records are ordinary durable log
//!   entries.
//!
//! For backwards compatibility with existing CLI scripts, [`FabricReceipt::decode`]
//! also accepts the legacy `asb ack` spelling (`v=1 t=<ms> re=<off> state=<verdict>`),
//! while [`FabricReceipt::encode`] always emits the canonical R8 grammar.
//!
//! ## Formal Verification (Embedded Clean Island + TLA+ Specification)
//!
//! The `clean { ... }` block below embeds the TLA+ state machine
//! `Astream.Wire.TlaFabricReceipts` and proves in the Clean CIC kernel:
//! 1. `spec_satisfies_receipt_safety`:
//!    `(Spec dl t0) ⊨ □ (Lift ReceiptSafetyInv)` — across all reachable behaviors,
//!    a pending request transitions at most once to either `Acked` (with a valid
//!    non-zero verdict) or `Expired` (only when `dlMs <= btMs <= nowMs`), and
//!    terminal states are mutually exclusive and immutable under clock ticks.
//! 2. `deadline_elapsed`: kernel-certified two-language contract (`clean-kernel-defeq`)
//!    proving `dl_ms <= bt_ms` matches `deadline_elapsed_isl`.

use crate::Offset;
use core::fmt;

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
#[path = "receipt_island.rs"]
mod receipt_island;
#[cfg(not(clean_islands))]
pub use crate::twins::monotone_clock_tick;
#[cfg(clean_islands)]
pub use receipt_island::monotone_clock_tick;

contract_fn! {
    /// Kernel-certified deadline-elapsed check (`dl_ms <= bt_ms`), backed by the
    /// Clean-origin [`monotone_clock_tick`] (`max(bt_ms, dl_ms) == bt_ms`).
    #[must_use]
    pub fn deadline_elapsed(dl_ms: u64, bt_ms: u64) -> bool
    clauses [ensures result == (dl_ms <= bt_ms)]
    {
        dl_ms <= bt_ms
    }
}

/// The three disposition verdicts of a fabric `ack` receipt (`REQUIREMENTS` R8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiptVerdict {
    /// The recipient processed and completed the request.
    Handled,
    /// The recipient explicitly refused the request.
    Refused,
    /// The recipient recorded the request for later execution.
    Deferred,
}

impl ReceiptVerdict {
    /// Canonical lowercase wire token (`handled`, `refused`, `deferred`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Handled => "handled",
            Self::Refused => "refused",
            Self::Deferred => "deferred",
        }
    }

    /// Parse a canonical verdict token.
    pub fn parse(s: &str) -> Result<Self, ReceiptError> {
        match s {
            "handled" => Ok(Self::Handled),
            "refused" => Ok(Self::Refused),
            "deferred" => Ok(Self::Deferred),
            _ => Err(ReceiptError::InvalidVerdict),
        }
    }
}

impl fmt::Display for ReceiptVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a fabric receipt or deadline-expiry body failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptError {
    /// First token was neither `ack`, `expired`, nor legacy `v=1`.
    UnknownKind,
    /// A required field (`re=`, `verdict=`, `dl=`, `bt=`) was missing or repeated.
    MissingOrDuplicateField(&'static str),
    /// An unknown `key=value` token appeared in the body.
    UnknownField,
    /// A numeric field (`re=`, `dl=`, `bt=`) was not a valid unsigned 64-bit integer.
    InvalidNumber(&'static str),
    /// The `verdict=` value was not `handled|refused|deferred`.
    InvalidVerdict,
    /// An `expired` receipt had `bt < dl` (emitted before the deadline elapsed).
    DeadlineNotElapsed { dl_ms: u64, bt_ms: u64 },
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind => write!(f, "expected 'ack' or 'expired' receipt header"),
            Self::MissingOrDuplicateField(k) => {
                write!(f, "missing or duplicate receipt field '{k}'")
            }
            Self::UnknownField => write!(f, "unknown field in receipt body"),
            Self::InvalidNumber(k) => write!(f, "invalid u64 in receipt field '{k}'"),
            Self::InvalidVerdict => {
                write!(
                    f,
                    "invalid receipt verdict (expected handled|refused|deferred)"
                )
            }
            Self::DeadlineNotElapsed { dl_ms, bt_ms } => write!(
                f,
                "expired receipt timestamp bt={bt_ms} is before deadline dl={dl_ms}"
            ),
        }
    }
}

impl std::error::Error for ReceiptError {}

/// A canonical fabric receipt or deadline-expiry record body (`REQUIREMENTS` R8).
#[derive(Debug, Clone, Eq)]
pub enum FabricReceipt {
    /// `ack re=<off> verdict=handled|refused|deferred bt=<ms>`
    Ack {
        /// Log offset of the message being acknowledged.
        re: Offset,
        /// Disposition verdict (`handled`, `refused`, or `deferred`).
        verdict: ReceiptVerdict,
        /// Broker/bridge timestamp in unix milliseconds.
        bt_ms: u64,
    },
    /// `expired re=<off> dl=<ms> bt=<ms>`
    Expired {
        /// Log offset of the message whose deadline elapsed without an answer/report/ack.
        re: Offset,
        /// The original deadline in unix milliseconds.
        dl_ms: u64,
        /// Broker/bridge timestamp in unix milliseconds when expiry was recorded (`bt_ms >= dl_ms`).
        bt_ms: u64,
    },
}

impl PartialEq for FabricReceipt {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Ack {
                    re: r1,
                    verdict: v1,
                    bt_ms: b1,
                },
                Self::Ack {
                    re: r2,
                    verdict: v2,
                    bt_ms: b2,
                },
            ) => r1.0 == r2.0 && *v1 == *v2 && *b1 == *b2,
            (
                Self::Expired {
                    re: r1,
                    dl_ms: d1,
                    bt_ms: b1,
                },
                Self::Expired {
                    re: r2,
                    dl_ms: d2,
                    bt_ms: b2,
                },
            ) => r1.0 == r2.0 && *d1 == *d2 && *b1 == *b2,
            (Self::Ack { .. }, Self::Expired { .. }) | (Self::Expired { .. }, Self::Ack { .. }) => {
                false
            }
        }
    }
}

fn parse_u64_ascii(val: &str, field: &'static str) -> Result<u64, ReceiptError> {
    let bytes = val.as_bytes();
    if bytes.is_empty() {
        return Err(ReceiptError::InvalidNumber(field));
    }
    let mut acc = 0u64;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return Err(ReceiptError::InvalidNumber(field));
        }
        // For ASCII `'0'..='9'` (`0x30..=0x39`), the low nibble `b & 15` equals `b - b'0'`.
        let digit = (b & 15) as u64;
        let Some(mul) = acc.checked_mul(10) else {
            return Err(ReceiptError::InvalidNumber(field));
        };
        let Some(next) = mul.checked_add(digit) else {
            return Err(ReceiptError::InvalidNumber(field));
        };
        acc = next;
    }
    Ok(acc)
}

impl FabricReceipt {
    /// Advance a bridge/broker monotone clock `now_ms` by `sample_ms`
    /// (`max(now_ms, sample_ms)`), backed directly by the Clean-origin
    /// [`monotone_clock_tick`] function used in `TlaFabricReceipts.TickClock`.
    #[must_use]
    pub const fn tick_clock(now_ms: u64, sample_ms: u64) -> u64 {
        monotone_clock_tick(now_ms, sample_ms)
    }

    /// Construct a validated `expired` receipt, checking that `bt_ms >= dl_ms`
    /// ([`deadline_elapsed`]).
    pub fn new_expired(re: Offset, dl_ms: u64, bt_ms: u64) -> Result<Self, ReceiptError> {
        if !deadline_elapsed(dl_ms, bt_ms) {
            return Err(ReceiptError::DeadlineNotElapsed { dl_ms, bt_ms });
        }
        Ok(Self::Expired { re, dl_ms, bt_ms })
    }

    /// Referenced message offset (`re=<off>`).
    #[must_use]
    pub const fn re(&self) -> Offset {
        match self {
            Self::Ack { re, .. } | Self::Expired { re, .. } => *re,
        }
    }

    /// Timestamp (`bt=<ms>`) carried by the receipt.
    #[must_use]
    pub const fn bt_ms(&self) -> u64 {
        match self {
            Self::Ack { bt_ms, .. } | Self::Expired { bt_ms, .. } => *bt_ms,
        }
    }

    /// Format into the canonical `REQUIREMENTS` R8 wire string.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::Ack { re, verdict, bt_ms } => {
                format!("ack re={} verdict={} bt={bt_ms}", re.0, verdict.as_str())
            }
            Self::Expired { re, dl_ms, bt_ms } => {
                format!("expired re={} dl={dl_ms} bt={bt_ms}", re.0)
            }
        }
    }

    /// Decode a canonical R8 receipt (`ack re=...` or `expired re=...`) or legacy
    /// `asb ack` body (`v=1 t=... re=... state=...`).
    pub fn decode(input: &str) -> Result<Self, ReceiptError> {
        let line = input.trim_end_matches(['\r', '\n']);
        let mut tokens = line.split(' ');
        let Some(first) = tokens.next() else {
            return Err(ReceiptError::UnknownKind);
        };
        match first {
            "ack" => {
                let mut re: Option<Offset> = None;
                let mut verdict: Option<ReceiptVerdict> = None;
                let mut bt_ms: Option<u64> = None;
                for tok in tokens {
                    if let Some(v) = tok.strip_prefix("re=") {
                        if re.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("re"));
                        }
                        re = Some(Offset(parse_u64_ascii(v, "re")?));
                    } else if let Some(v) = tok.strip_prefix("verdict=") {
                        if verdict.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("verdict"));
                        }
                        verdict = Some(ReceiptVerdict::parse(v)?);
                    } else if let Some(v) = tok.strip_prefix("bt=") {
                        if bt_ms.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("bt"));
                        }
                        bt_ms = Some(parse_u64_ascii(v, "bt")?);
                    } else {
                        return Err(ReceiptError::UnknownField);
                    }
                }
                Ok(Self::Ack {
                    re: re.ok_or(ReceiptError::MissingOrDuplicateField("re"))?,
                    verdict: verdict.ok_or(ReceiptError::MissingOrDuplicateField("verdict"))?,
                    bt_ms: bt_ms.ok_or(ReceiptError::MissingOrDuplicateField("bt"))?,
                })
            }
            "expired" => {
                let mut re: Option<Offset> = None;
                let mut dl_ms: Option<u64> = None;
                let mut bt_ms: Option<u64> = None;
                for tok in tokens {
                    if let Some(v) = tok.strip_prefix("re=") {
                        if re.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("re"));
                        }
                        re = Some(Offset(parse_u64_ascii(v, "re")?));
                    } else if let Some(v) = tok.strip_prefix("dl=") {
                        if dl_ms.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("dl"));
                        }
                        dl_ms = Some(parse_u64_ascii(v, "dl")?);
                    } else if let Some(v) = tok.strip_prefix("bt=") {
                        if bt_ms.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("bt"));
                        }
                        bt_ms = Some(parse_u64_ascii(v, "bt")?);
                    } else {
                        return Err(ReceiptError::UnknownField);
                    }
                }
                let re = re.ok_or(ReceiptError::MissingOrDuplicateField("re"))?;
                let dl_ms = dl_ms.ok_or(ReceiptError::MissingOrDuplicateField("dl"))?;
                let bt_ms = bt_ms.ok_or(ReceiptError::MissingOrDuplicateField("bt"))?;
                Self::new_expired(re, dl_ms, bt_ms)
            }
            "v=1" => {
                let mut re: Option<Offset> = None;
                let mut verdict: Option<ReceiptVerdict> = None;
                let mut bt_ms: Option<u64> = None;
                for tok in tokens {
                    if let Some(v) = tok.strip_prefix("re=") {
                        if re.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("re"));
                        }
                        re = Some(Offset(parse_u64_ascii(v, "re")?));
                    } else if let Some(v) = tok.strip_prefix("state=") {
                        if verdict.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("verdict"));
                        }
                        verdict = Some(ReceiptVerdict::parse(v)?);
                    } else if let Some(v) = tok.strip_prefix("t=") {
                        if bt_ms.is_some() {
                            return Err(ReceiptError::MissingOrDuplicateField("bt"));
                        }
                        bt_ms = Some(parse_u64_ascii(v, "bt")?);
                    } else {
                        return Err(ReceiptError::UnknownField);
                    }
                }
                Ok(Self::Ack {
                    re: re.ok_or(ReceiptError::MissingOrDuplicateField("re"))?,
                    verdict: verdict.ok_or(ReceiptError::MissingOrDuplicateField("verdict"))?,
                    bt_ms: bt_ms.ok_or(ReceiptError::MissingOrDuplicateField("bt"))?,
                })
            }
            _ => Err(ReceiptError::UnknownKind),
        }
    }
}

impl fmt::Display for FabricReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r8_ack_and_expired_grammar_conformance() {
        for (v_str, v_enum) in [
            ("handled", ReceiptVerdict::Handled),
            ("refused", ReceiptVerdict::Refused),
            ("deferred", ReceiptVerdict::Deferred),
        ] {
            let wire = format!("ack re=42 verdict={v_str} bt=1700000000123");
            let decoded = FabricReceipt::decode(&wire).unwrap();
            assert_eq!(
                decoded,
                FabricReceipt::Ack {
                    re: Offset(42),
                    verdict: v_enum,
                    bt_ms: 1_700_000_000_123,
                }
            );
            assert_eq!(decoded.encode(), wire);
            assert_eq!(decoded.re(), Offset(42));
            assert_eq!(decoded.bt_ms(), 1_700_000_000_123);
        }

        let exp_wire = "expired re=99 dl=1700000005000 bt=1700000005000";
        let decoded_exp = FabricReceipt::decode(exp_wire).unwrap();
        assert_eq!(
            decoded_exp,
            FabricReceipt::Expired {
                re: Offset(99),
                dl_ms: 1_700_000_005_000,
                bt_ms: 1_700_000_005_000,
            }
        );
        assert_eq!(decoded_exp.encode(), exp_wire);

        // Premature expiry (`bt < dl`) is rejected by `deadline_elapsed`.
        assert_eq!(
            FabricReceipt::decode("expired re=99 dl=1700000005000 bt=1700000004999"),
            Err(ReceiptError::DeadlineNotElapsed {
                dl_ms: 1_700_000_005_000,
                bt_ms: 1_700_000_004_999,
            })
        );

        // Legacy `asb ack` format also decodes into `FabricReceipt::Ack`.
        let legacy = "v=1 t=1700000000999 re=7 state=handled";
        assert_eq!(
            FabricReceipt::decode(legacy).unwrap(),
            FabricReceipt::Ack {
                re: Offset(7),
                verdict: ReceiptVerdict::Handled,
                bt_ms: 1_700_000_000_999,
            }
        );
    }

    #[test]
    fn rejects_malformed_receipts() {
        assert_eq!(
            FabricReceipt::decode("unknown re=1"),
            Err(ReceiptError::UnknownKind)
        );
        assert_eq!(
            FabricReceipt::decode("ack re=1 verdict=maybe bt=10"),
            Err(ReceiptError::InvalidVerdict)
        );
        assert_eq!(
            FabricReceipt::decode("ack re=1 re=2 verdict=handled bt=10"),
            Err(ReceiptError::MissingOrDuplicateField("re"))
        );
        assert_eq!(
            FabricReceipt::decode("ack re=+1 verdict=handled bt=10"),
            Err(ReceiptError::InvalidNumber("re"))
        );
    }

    #[test]
    fn clean_origin_monotone_clock_tick_advances_monotonically() {
        assert_eq!(FabricReceipt::tick_clock(1_000, 950), 1_000);
        assert_eq!(FabricReceipt::tick_clock(1_000, 1_000), 1_000);
        assert_eq!(FabricReceipt::tick_clock(1_000, 1_250), 1_250);
    }
}
