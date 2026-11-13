//! Closed selectors for actual retained seed-input work. These are DATA only;
//! decoding does not construct a Root, source/ODB owner or completed producer.
use std::collections::BTreeSet;
use std::io;

use super::{MAX_SOURCES, Plan};
use crate::wire::{Decoder, Encoder, invalid};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub source: u32,
    pub odb: u32,
    /// Empty is this family's root; others are exact committed gitlink paths.
    pub relative: Vec<u8>,
    pub commit: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub source: Vec<Provider>,
    pub orchestration: Vec<Provider>,
}
fn commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Selection {
    pub fn validate(&self, plan: &Plan) -> io::Result<()> {
        for family in [&self.source, &self.orchestration] {
            if family.is_empty() || family.len() > MAX_SOURCES || !family[0].relative.is_empty() {
                return Err(invalid());
            }
            let root = plan
                .sources
                .get(family[0].source as usize)
                .ok_or_else(invalid)?;
            let width = family[0].commit.len();
            let mut seen = BTreeSet::new();
            for item in family {
                let source = plan.sources.get(item.source as usize).ok_or_else(invalid)?;
                if item.odb as usize >= plan.sources.len()
                    || !commit(&item.commit)
                    || item.commit.len() != width
                    || item.relative.len() > 4096
                    || item.relative.contains(&0)
                    || !seen.insert(&item.relative)
                    || (!item.relative.is_empty()
                        && item
                            .relative
                            .split(|b| *b == b'/')
                            .any(|part| part.is_empty() || part == b"." || part == b".."))
                {
                    return Err(invalid());
                }
                let mut expected = root.root.clone();
                if !item.relative.is_empty() {
                    expected.push(b'/');
                    expected.extend(&item.relative);
                }
                if source.root != expected {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
    pub(super) fn write(&self, encoder: &mut Encoder) -> io::Result<()> {
        for family in [&self.source, &self.orchestration] {
            encoder.number(family.len() as u64);
            for item in family {
                encoder.number(item.source.into());
                encoder.number(item.odb.into());
                encoder.bytes(&item.relative)?;
                encoder.bytes(item.commit.as_bytes())?;
            }
        }
        Ok(())
    }
    pub(super) fn read(decoder: &mut Decoder<'_>) -> io::Result<Self> {
        fn family(decoder: &mut Decoder<'_>) -> io::Result<Vec<Provider>> {
            let count = decoder.count(MAX_SOURCES)?;
            let mut values = Vec::new();
            values.try_reserve_exact(count).map_err(|_| invalid())?;
            for _ in 0..count {
                values.push(Provider {
                    source: decoder.narrow()?,
                    odb: decoder.narrow()?,
                    relative: decoder.bytes()?,
                    commit: decoder.text()?,
                });
            }
            Ok(values)
        }
        Ok(Self {
            source: family(decoder)?,
            orchestration: family(decoder)?,
        })
    }
}

#[cfg(test)]
mod tests;
