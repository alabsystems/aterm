// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Negotiated original-owning-wait observations attached to this exact existing
//! lifecycle binding. Decoding grants no execution or resource authority.
use super::{Binding, Decoder, Encoder, Event, EventFrame, MAX_PAYLOAD, bad, header};
use std::io;
pub use trust_resource_controller::accounting::{
    CpuTimes, Observation as AccountingObservation, Scope as AccountingScope,
};

/// Accounted callers require this distinct terminal after actual settlement.
/// Legacy earlier RootFinished bytes cannot stand in for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountedEventFrame {
    /// Original status, cause and duration with its existing exact binding.
    pub finished: EventFrame,
    /// Data from the same owning wait; absent fields remain uncollected.
    pub observation: AccountingObservation,
}

fn nested(e: &mut Encoder, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_PAYLOAD {
        return Err(bad());
    }
    e.number(bytes.len() as u64);
    e.0.extend_from_slice(bytes);
    Ok(())
}
fn read_nested(d: &mut Decoder<'_>) -> io::Result<Vec<u8>> {
    let count = usize::try_from(d.number()?).map_err(|_| bad())?;
    if count == 0 || count > MAX_PAYLOAD {
        return Err(bad());
    }
    Ok(d.take(count)?.to_vec())
}
impl AccountedEventFrame {
    /// Encode the distinct terminal without altering legacy event bytes.
    ///
    /// # Errors
    /// A nonterminal event, invented refusal counters or frame bounds differ.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        match &self.finished.event {
            Event::Finished { .. } => {}
            Event::Refused { .. } if self.observation == AccountingObservation::uncollected() => {}
            _ => return Err(bad()),
        }
        let mut e = Encoder::new(4, &self.finished.binding)?;
        e.number(1);
        nested(&mut e, &self.finished.encode()?)?;
        nested(&mut e, &self.observation.encode()?)?;
        e.finish()
    }
    /// Decode this exact attempt's terminal data, granting no custody fact.
    ///
    /// # Errors
    /// Binding, version, scope, phase, bounds, truncation or trailing bytes differ.
    pub fn decode(bytes: &[u8], expected: &Binding) -> io::Result<Self> {
        let (binding, mut d) = header(bytes, 4)?;
        if &binding != expected || d.number()? != 1 {
            return Err(bad());
        }
        let finished = EventFrame::decode(&read_nested(&mut d)?, expected)?;
        let observation = AccountingObservation::decode(&read_nested(&mut d)?)?;
        d.done()?;
        let result = Self {
            finished,
            observation,
        };
        result.encode()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_admission::{CompletionCause, Outcome};

    #[test]
    fn account_terminal_is_bound_distinct_from_legacy_and_keeps_absent_or_observed_zero() {
        let binding = Binding {
            session: "s-abcd".into(),
            attempt: [1; 16],
        };
        let plain = EventFrame {
            binding: binding.clone(),
            event: Event::Finished {
                outcome: Outcome::Signal(9),
                cause: CompletionCause::Cancelled,
                runtime_ns: 55,
            },
        };
        for observation in [
            AccountingObservation::uncollected(),
            AccountingObservation {
                scope: AccountingScope::ChildAndWaitedDescendantsV1,
                cpu_times: Some(CpuTimes {
                    user_ns: 0,
                    system_ns: 0,
                }),
                peak_rss_bytes: Some(0),
            },
        ] {
            let frame = AccountedEventFrame {
                finished: plain.clone(),
                observation,
            };
            let bytes = frame.encode().unwrap();
            assert_eq!(
                AccountedEventFrame::decode(&bytes, &binding).unwrap(),
                frame
            );
            assert!(EventFrame::decode(&bytes, &binding).is_err());
            assert!(AccountedEventFrame::decode(&plain.encode().unwrap(), &binding).is_err());
            let foreign = Binding {
                attempt: [2; 16],
                ..binding.clone()
            };
            assert!(AccountedEventFrame::decode(&bytes, &foreign).is_err());
            for end in 0..bytes.len() {
                assert!(AccountedEventFrame::decode(&bytes[..end], &binding).is_err());
            }
            let mut extra = bytes;
            extra.push(0);
            assert!(AccountedEventFrame::decode(&extra, &binding).is_err());
        }
    }
}
