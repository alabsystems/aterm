//! Ordinary whole-repository source-publication declarations, never authority.
//!
//! Exact commits and the complete declared check population are equality DATA.
//! A consumer still needs actual current source qualification and independently
//! admitted repository/Git/Agh owners. Improvement Land keeps its own policy.
use super::*;

pub const MAIN_REF: &str = "refs/heads/main";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    WholeRepository { root: Vec<u8> },
}
impl Target {
    pub fn root(&self) -> &[u8] {
        match self {
            Self::WholeRepository { root } => root,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredCheck {
    pub id: String,
    pub node: NodeId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePolicy {
    pub target: Target,
    pub candidate_commit: String,
    pub expected_main_commit: String,
    pub required_checks: Vec<RequiredCheck>,
}

fn commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value.bytes().any(|byte| byte != b'0')
}
fn check_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

impl SourcePolicy {
    fn syntax(&self) -> io::Result<()> {
        if !path(self.target.root())
            || !commit(&self.candidate_commit)
            || !commit(&self.expected_main_commit)
            || self.candidate_commit.len() != self.expected_main_commit.len()
            || self.candidate_commit == self.expected_main_commit
            || self.required_checks.is_empty()
            || self.required_checks.len() > MAX_NODES
        {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        let mut nodes = BTreeSet::new();
        for check in &self.required_checks {
            if !check_id(&check.id) || !ids.insert(&check.id) || !nodes.insert(check.node) {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Validate only the declaration and its exact direct successful-check
    /// edges. The maintained source qualifier must independently establish
    /// which checks the complete current repository delta actually requires.
    pub fn validate(&self, plan: &Plan, publication: NodeId) -> io::Result<()> {
        self.syntax()?;
        let node = plan.nodes.get(publication.0 as usize).ok_or_else(invalid)?;
        if !matches!(&node.kind, Kind::PublishSource { policy } if policy == self)
            || node.dependencies.len() != self.required_checks.len()
            || plan
                .sources
                .iter()
                .any(|source| !within(self.target.root(), &source.root))
        {
            return Err(invalid());
        }
        for check in &self.required_checks {
            let required = plan.nodes.get(check.node.0 as usize).ok_or_else(invalid)?;
            if check.node == publication
                || !matches!(
                    required.kind,
                    Kind::Compile { .. } | Kind::Verify { .. } | Kind::Tests { .. }
                )
                || !node.dependencies.iter().any(|dependency| {
                    dependency.node == check.node
                        && dependency.condition == EdgeCondition::Succeeded
                })
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub(in crate::native_job) fn write(&self, e: &mut Encoder) -> io::Result<()> {
        self.syntax()?;
        e.number(1);
        match &self.target {
            Target::WholeRepository { root } => {
                e.number(0);
                e.bytes(root)?;
            }
        }
        e.bytes(self.candidate_commit.as_bytes())?;
        e.bytes(self.expected_main_commit.as_bytes())?;
        e.number(self.required_checks.len() as u64);
        for check in &self.required_checks {
            e.bytes(check.id.as_bytes())?;
            e.number(check.node.0.into());
        }
        Ok(())
    }

    pub(in crate::native_job) fn read(d: &mut Decoder<'_>) -> io::Result<Self> {
        if d.number()? != 1 {
            return Err(invalid());
        }
        let target = match d.number()? {
            0 => Target::WholeRepository { root: d.bytes()? },
            _ => return Err(invalid()),
        };
        let candidate_commit = d.text()?;
        let expected_main_commit = d.text()?;
        let count = d.count(MAX_NODES)?;
        let mut required_checks = Vec::new();
        required_checks
            .try_reserve_exact(count)
            .map_err(|_| invalid())?;
        for _ in 0..count {
            required_checks.push(RequiredCheck {
                id: d.text()?,
                node: NodeId(d.narrow()?),
            });
        }
        let value = Self {
            target,
            candidate_commit,
            expected_main_commit,
            required_checks,
        };
        value.syntax()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests;
