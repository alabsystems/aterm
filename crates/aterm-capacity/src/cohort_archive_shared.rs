// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reuse only the actual hot publisher's original descriptions. The fresh
//! history reader still independently reads and judges every protocol byte.

use super::*;

impl Inventory {
    pub(super) fn share_root(&self, named: &File) -> io::Result<Arc<File>> {
        self.recheck()?;
        if identity(named, true)? != identity(&self.root, true)?
            || directory_origin(named)? != self.origin
        {
            return Err(bad("hot archive root differs from its original inventory"));
        }
        Ok(Arc::clone(&self.root))
    }

    pub(super) fn share_directory(
        &self,
        parent: &Arc<File>,
        name: &str,
        named: &File,
    ) -> io::Result<Arc<File>> {
        if !Arc::ptr_eq(parent, &self.root)
            || identity(parent, true)? != identity(&self.root, true)?
            || directory_origin(parent)? != self.origin
        {
            return Err(bad("hot archive directory has another original parent"));
        }
        let original = self
            .directories
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| bad("hot archive directory was not in the original inventory"))?;
        if !Arc::ptr_eq(&original.parent, parent)
            || identity(named, true)? != original.identity
            || identity(&original.file, true)? != original.identity
            || directory_origin(named)? != original.origin
            || directory_origin(&original.file)? != original.origin
        {
            return Err(bad("hot archive directory changed from its original input"));
        }
        // Re-open the named entry again. The caller's first named observation
        // is a bookend, not permission to substitute a different directory.
        let after = open(parent, name, false, true)?;
        if identity(&after, true)? != original.identity
            || directory_origin(&after)? != original.origin
        {
            return Err(bad("hot archive directory changed during selection"));
        }
        Ok(Arc::clone(&original.file))
    }

    pub(super) fn share_member(
        &self,
        parent: &Arc<File>,
        name: &str,
        sealed: bool,
    ) -> io::Result<(Arc<File>, String, Stamp)> {
        // Only the actual retained parent from this finite hot inventory is
        // eligible. Physical identity remains checked, not inferred from Arc.
        let parent_identity = identity(parent, true)?;
        let mut candidates = self
            .files
            .iter()
            .filter(|entry| entry.name == name && Arc::ptr_eq(&entry.parent, parent));
        let original = candidates
            .next()
            .ok_or_else(|| bad("hot archive member lacks its original parent and name"))?;
        if candidates.next().is_some()
            || identity(&original.parent, true)? != parent_identity
            || Stamp::read(&original.file)? != original.stamp
            || (sealed && original.file.metadata()?.mode() & 0o7777 != 0o400)
        {
            return Err(bad("hot archive member changed from its original input"));
        }
        {
            let named = open(parent, name, false, false)?;
            if Stamp::read(&named)? != original.stamp {
                return Err(bad(
                    "hot archive member name no longer selects its original",
                ));
            }
        }
        // Read at explicit offsets through the SAME held description. This
        // is a new bounded read and stamp check, not cached Inventory bytes.
        let (body, stamp) = bytes(&original.file, MAX_ENTRY)?;
        if stamp != original.stamp
            || Stamp::read(&open(parent, name, false, false)?)? != stamp
            || (sealed && stamp.mode() & 0o7777 != 0o400)
        {
            return Err(bad(
                "hot archive member changed during its fresh semantic read",
            ));
        }
        Ok((Arc::clone(&original.file), body, stamp))
    }
}
