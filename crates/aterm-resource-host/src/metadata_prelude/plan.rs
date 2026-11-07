// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private exec data, deliberately containing no admission binding or budget.
use super::*;
use trust_resource_controller::CommandSpec;
const MAGIC: &[u8; 8] = b"AMEX0001";
const MAX_ITEMS: usize = 4096;
pub(super) struct ExecPlan {
    pub parent: transport::Peer,
    pub command: CommandSpec,
    pub targets: Vec<i32>,
}
impl ExecPlan {
    fn validate(&self) -> io::Result<()> {
        if self.parent.pid <= 1
            || self.parent.birth == 0
            || self.command.arg0.is_some()
            || self.command.program.first() != Some(&b'/')
            || self.command.cwd.first() != Some(&b'/')
            || self.command.args.len() > MAX_ITEMS
            || self.command.environment.len() > MAX_ITEMS
            || self.targets.len() > 3
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
        let program = read.field()?;
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
        for _ in 0..read.count(3)? {
            targets.push(i32::try_from(read.number()?).map_err(|_| refused())?);
        }
        if !read.0.is_empty() {
            return Err(refused());
        }
        let plan = Self {
            parent,
            command: CommandSpec {
                program,
                arg0: None,
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
            parent: transport::Peer {
                pid: 123,
                birth: 456,
            },
            targets: vec![3, 4],
            command: CommandSpec {
                program: b"/native/bin/query".to_vec(),
                arg0: None,
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
        for end in 0..bytes.len() {
            assert!(ExecPlan::decode(&bytes[..end]).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(ExecPlan::decode(&extra).is_err());
        assert!(aterm_types::resource_admission::Submission::decode(&bytes).is_err());
        let mut changed = decoded;
        changed.command.environment[0].1.push(0);
        assert!(changed.encode().is_err());
        changed.command.environment[0].1 = vec![b'x'; MAX_PLAN + 1];
        assert!(changed.encode().is_err());
    }
}
