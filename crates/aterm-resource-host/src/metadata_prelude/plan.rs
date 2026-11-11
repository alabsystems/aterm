// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private launch data transports existing command state; it mints no admission grant.
use super::*;
use trust_resource_controller::CommandSpec;
const MAGIC: &[u8; 8] = b"AMEX0004";
const MAX_ITEMS: usize = 4096;
// Actual original GNU pair + SDK keeper + readonly source pair + close-only
// output directory/lock. This bound supplies no description or resource grant.
const MAX_TARGETS: usize = 7;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Seal {
    NoFork,
    // Qualification only: no production recipe may defer its containment.
    #[cfg(all(test, target_os = "macos"))]
    Child,
}
pub(super) struct ExecPlan {
    pub parent: transport::Peer,
    pub command: CommandSpec,
    pub targets: Vec<i32>,
    pub footprint_mib: Option<u32>,
    pub seal: Seal,
}
impl ExecPlan {
    fn validate(&self) -> io::Result<()> {
        #[cfg(all(test, target_os = "macos"))]
        if self.seal == Seal::Child && self.footprint_mib.is_none() {
            return Err(refused());
        }
        if let Some(mib) = self.footprint_mib {
            #[cfg(target_os = "macos")]
            spawnfd::FootprintLimit::new(mib).map_err(|_| refused())?;
            #[cfg(not(target_os = "macos"))]
            {
                let _ = mib;
                return Err(io::ErrorKind::Unsupported.into());
            }
        }
        if self.parent.pid <= 1
            || self.parent.birth == 0
            || self.command.program.first() != Some(&b'/')
            || self.command.cwd.first() != Some(&b'/')
            || self.command.args.len() > MAX_ITEMS
            || self.command.environment.len() > MAX_ITEMS
            || self.targets.len() > MAX_TARGETS
        {
            return Err(refused());
        }
        let mut targets = std::collections::BTreeSet::new();
        if self
            .targets
            .iter()
            .any(|target| *target < 3 || !targets.insert(target))
        {
            return Err(refused());
        }
        let mut keys = std::collections::BTreeSet::new();
        for (key, _) in &self.command.environment {
            if key.is_empty() || key.contains(&b'=') || !keys.insert(key) {
                return Err(refused());
            }
        }
        Ok(())
    }
    pub(super) fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = Vec::from(&MAGIC[..]);
        bytes.extend(self.parent.pid.to_be_bytes());
        bytes.extend(self.parent.birth.to_be_bytes());
        match self.footprint_mib {
            None => bytes.push(0),
            Some(mib) => {
                bytes.push(1);
                bytes.extend(mib.to_be_bytes());
            }
        }
        fn field(out: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
            if value.len() > MAX_PLAN
                || value.contains(&0)
                || out.len().saturating_add(4).saturating_add(value.len()) > MAX_PLAN
            {
                return Err(refused());
            }
            out.extend((value.len() as u32).to_be_bytes());
            out.extend(value);
            Ok(())
        }
        field(&mut bytes, &self.command.program)?;
        match &self.command.arg0 {
            None => bytes.push(0),
            Some(value) => {
                bytes.push(1);
                field(&mut bytes, value)?;
            }
        }
        field(&mut bytes, &self.command.cwd)?;
        bytes.extend((self.command.args.len() as u32).to_be_bytes());
        for argument in &self.command.args {
            field(&mut bytes, argument)?;
        }
        bytes.extend((self.command.environment.len() as u32).to_be_bytes());
        for (key, value) in &self.command.environment {
            field(&mut bytes, key)?;
            field(&mut bytes, value)?;
        }
        bytes.extend((self.targets.len() as u32).to_be_bytes());
        for target in &self.targets {
            bytes.extend((*target as u32).to_be_bytes());
        }
        bytes.push(match self.seal {
            Seal::NoFork => 0,
            #[cfg(all(test, target_os = "macos"))]
            Seal::Child => 1,
        });
        if bytes.len() > MAX_PLAN {
            return Err(refused());
        }
        Ok(bytes)
    }
    pub(super) fn decode(bytes: &[u8]) -> io::Result<Self> {
        struct Reader<'a>(&'a [u8]);
        impl Reader<'_> {
            fn take(&mut self, length: usize) -> io::Result<&[u8]> {
                let (value, rest) = self.0.split_at_checked(length).ok_or_else(refused)?;
                self.0 = rest;
                Ok(value)
            }
            fn number(&mut self) -> io::Result<u32> {
                Ok(u32::from_be_bytes(
                    self.take(4)?.try_into().map_err(|_| refused())?,
                ))
            }
            fn count(&mut self, maximum: usize) -> io::Result<usize> {
                let count = self.number()? as usize;
                if count > maximum {
                    Err(refused())
                } else {
                    Ok(count)
                }
            }
            fn field(&mut self) -> io::Result<Vec<u8>> {
                let length = self.count(MAX_PLAN)?;
                let value = self.take(length)?;
                if value.contains(&0) {
                    return Err(refused());
                }
                Ok(value.to_vec())
            }
        }
        if bytes.len() > MAX_PLAN {
            return Err(refused());
        }
        let mut read = Reader(bytes);
        if read.take(8)? != MAGIC {
            return Err(refused());
        }
        let parent = transport::Peer {
            pid: read.number()?,
            birth: u64::from_be_bytes(read.take(8)?.try_into().map_err(|_| refused())?),
        };
        let footprint_mib = match read.take(1)?[0] {
            0 => None,
            1 => Some(read.number()?),
            _ => return Err(refused()),
        };
        let program = read.field()?;
        let arg0 = match read.take(1)?[0] {
            0 => None,
            1 => Some(read.field()?),
            _ => return Err(refused()),
        };
        let cwd = read.field()?;
        let mut args = Vec::new();
        for _ in 0..read.count(MAX_ITEMS)? {
            args.push(read.field()?);
        }
        let mut environment = Vec::new();
        for _ in 0..read.count(MAX_ITEMS)? {
            environment.push((read.field()?, read.field()?));
        }
        let mut targets = Vec::new();
        for _ in 0..read.count(MAX_TARGETS)? {
            targets.push(i32::try_from(read.number()?).map_err(|_| refused())?);
        }
        let seal = match read.take(1)?[0] {
            0 => Seal::NoFork,
            #[cfg(all(test, target_os = "macos"))]
            1 => Seal::Child,
            _ => return Err(refused()),
        };
        if !read.0.is_empty() {
            return Err(refused());
        }
        let plan = Self {
            parent,
            footprint_mib,
            seal,
            command: CommandSpec {
                program,
                arg0,
                args,
                environment,
                cwd,
            },
            targets,
        };
        plan.validate()?;
        Ok(plan)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exec_data_is_lossless_bounded_and_contains_no_admission_fields() {
        let plan = ExecPlan {
            footprint_mib: None,
            seal: Seal::NoFork,
            parent: transport::Peer {
                pid: 123,
                birth: 456,
            },
            targets: vec![3, 4, 5, 6, 7, 8, 9],
            command: CommandSpec {
                program: b"/native/bin/query".to_vec(),
                arg0: Some(vec![b'x', 0xff]),
                args: vec![vec![b'-', 0xff]],
                environment: vec![(b"LANG".to_vec(), b"C".to_vec())],
                cwd: b"/private/work".to_vec(),
            },
        };
        let bytes = plan.encode().unwrap();
        let decoded = ExecPlan::decode(&bytes).unwrap();
        assert_eq!(decoded.parent, plan.parent);
        assert!(decoded.command == plan.command);
        assert_eq!(decoded.targets, plan.targets);
        assert_eq!(decoded.footprint_mib, plan.footprint_mib);
        assert!(decoded.seal == plan.seal);
        for end in 0..bytes.len() {
            assert!(ExecPlan::decode(&bytes[..end]).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(ExecPlan::decode(&extra).is_err());
        assert!(aterm_types::resource_admission::Submission::decode(&bytes).is_err());
        let mut old = bytes.clone();
        old[..8].copy_from_slice(b"AMEX0001");
        assert!(ExecPlan::decode(&old).is_err());
        old[..8].copy_from_slice(b"AMEX0002");
        assert!(ExecPlan::decode(&old).is_err());
        old[..8].copy_from_slice(b"AMEX0003");
        assert!(ExecPlan::decode(&old).is_err());
        let mut wrong_seal = bytes.clone();
        *wrong_seal.last_mut().unwrap() = 2;
        assert!(ExecPlan::decode(&wrong_seal).is_err());
        let mut unknown = bytes.clone();
        unknown[20] = 2;
        assert!(ExecPlan::decode(&unknown).is_err());
        for mib in [0, i32::MAX as u32 + 1, u32::MAX] {
            let mut invalid = bytes.clone();
            invalid[20] = 1;
            invalid.splice(21..21, mib.to_be_bytes());
            assert!(ExecPlan::decode(&invalid).is_err());
        }
        let mut limited = ExecPlan::decode(&bytes).unwrap();
        limited.footprint_mib = Some(64);
        #[cfg(target_os = "macos")]
        {
            let encoded = limited.encode().unwrap();
            assert_eq!(ExecPlan::decode(&encoded).unwrap().footprint_mib, Some(64));
            for end in 0..encoded.len() {
                assert!(ExecPlan::decode(&encoded[..end]).is_err());
            }
            let mut trailing = encoded;
            trailing.push(0);
            assert!(ExecPlan::decode(&trailing).is_err());
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            limited.encode().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let mut changed = decoded;
        changed.targets.push(10);
        assert!(changed.encode().is_err());
        changed.targets.pop();
        changed.command.arg0.as_mut().unwrap().push(0);
        assert!(changed.encode().is_err());
        changed.command.arg0.as_mut().unwrap().pop();
        changed.command.environment[0].1.push(0);
        assert!(changed.encode().is_err());
        changed.command.environment[0].1 = vec![b'x'; MAX_PLAN + 1];
        assert!(changed.encode().is_err());
    }
}
