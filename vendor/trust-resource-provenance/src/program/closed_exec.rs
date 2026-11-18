//! Actual retained inputs for a prospective fixed exec-name restriction.
//! This does not discover a complete chain, authorize argv, or close a child.

use super::*;
use std::ffi::CStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn native_supported() -> io::Result<()> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(())
    } else {
        Err(io::ErrorKind::Unsupported.into())
    }
}

fn native_names(names: &[Vec<u8>]) -> io::Result<Vec<CString>> {
    if names.is_empty()
        || names.len() > HeldExecImageSet::maximum_images()
        || names
            .iter()
            .any(|name| name.len() > trust_exec_policy::MAX_CLOSED_EXEC_NAME_BYTES)
    {
        return Err(refused("preparation exec name population exceeds bound"));
    }
    let strings = names
        .iter()
        .map(|name| CString::new(name.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| refused("preparation exec name contains NUL"))?;
    let borrowed = strings
        .iter()
        .map(|name| name.as_c_str())
        .collect::<Vec<_>>();
    trust_exec_policy::validate_closed_exec_names(&borrowed)?;
    Ok(strings)
}

trait Original: Send + Sync {
    fn checked_program(&self, end: Instant) -> io::Result<&HeldProgram>;
}
struct Retained<T> {
    original: Box<T>,
    checked: for<'a> fn(&'a T, Instant) -> io::Result<&'a HeldProgram>,
}
impl<T: Send + Sync> Original for Retained<T> {
    fn checked_program(&self, end: Instant) -> io::Result<&HeldProgram> {
        (self.checked)(&self.original, end)
    }
}

/// A lifetime adapter retaining the WHOLE supplied original owner. The checked
/// borrow cannot manufacture a HeldProgram from a path or observation. In
/// particular signed-tool callers retain their package/GC lease, rather than
/// recapturing its program pathname and dropping the stronger original owner.
/// This adapter is not evidence that a supplied population is a complete chain.
pub struct HeldExecImage {
    original: Box<dyn Original>,
    end: Instant,
    binding: Option<Binding>,
}
struct Binding {
    address: usize,
    name: CString,
}
impl HeldExecImage {
    /// `checked` must perform the existing owner's ordinary currentness checks
    /// and lend its SAME HeldProgram. Its own endpoint is still enforced there;
    /// `original_deadline` is an additional restrictive bound, never a renewal.
    /// No execution role or lifetime receipt is returned by retaining this box.
    pub fn retain<T: Send + Sync + 'static>(
        original: Box<T>,
        original_deadline: Instant,
        checked: for<'a> fn(&'a T, Instant) -> io::Result<&'a HeldProgram>,
    ) -> Self {
        Self {
            original: Box::new(Retained { original, checked }),
            end: original_deadline,
            binding: None,
        }
    }
    fn capture(&mut self, end: Instant) -> io::Result<()> {
        let end = end.min(self.end);
        time(end)?;
        let program = self.original.checked_program(end)?;
        program.validate(end)?;
        let name = CString::new(program.program().as_os_str().as_bytes())
            .map_err(|_| refused("held exec image name contains NUL"))?;
        self.binding = Some(Binding {
            address: std::ptr::from_ref(program) as usize,
            name,
        });
        self.check(end)
    }
    fn check(&self, end: Instant) -> io::Result<()> {
        self.checked_program(end).map(|_| ())
    }
    fn checked_program(&self, end: Instant) -> io::Result<&HeldProgram> {
        let end = end.min(self.end);
        time(end)?;
        let binding = self
            .binding
            .as_ref()
            .ok_or_else(|| refused("exec image has no original binding"))?;
        let program = self.original.checked_program(end)?;
        if binding.address != std::ptr::from_ref(program) as usize
            || binding.name.to_bytes() != program.program().as_os_str().as_bytes()
        {
            return Err(refused("exec image projection changed its original owner"));
        }
        program.validate(end)?;
        time(end)?;
        Ok(program)
    }
    fn name(&self) -> io::Result<&CStr> {
        self.binding
            .as_ref()
            .map(|value| value.name.as_c_str())
            .ok_or_else(|| refused("exec image has no original binding"))
    }
}

/// Host-retained physical originals for one finite exec-name policy. This is
/// neither a semantic selection/CLT issuer nor permission to launch. It retains
/// every whole original owner and rechecks its checked SAME program projection.
/// Complete actual population, original environment/search inputs, and exclusion
/// of concurrent same-UID name writers remain the operation issuer's duties.
///
/// Name filtering is not descriptor-based execution. As with HeldProgram,
/// validation rechecks stamps/identity/ACL; it is not another full SHA pass,
/// atomic namespace freeze, or protection against undetected drift+restore.
pub struct HeldExecImageSet {
    images: Vec<HeldExecImage>,
    credentials: Option<command_search::Credentials>,
    creator: u32,
    end: Instant,
    revoked: AtomicBool,
    attempted: AtomicBool,
}

/// Host-retained transfer inputs, not a receipt that a child installed policy.
/// Only a SAME live set and borrowed actual member can create this value. The
/// authenticated prelude receives restrictive name DATA; this original whole
/// owner remains outside self-exec through actual native scope settlement.
#[derive(Clone)]
pub struct PreparedExecTransfer {
    original: Arc<HeldExecImageSet>,
    initial: usize,
    end: Instant,
}
impl PreparedExecTransfer {
    pub fn original_deadline(&self) -> Instant {
        self.end
    }
    pub fn check_current(&self) -> io::Result<()> {
        let mut checking = Checking {
            revoked: &self.original.revoked,
            complete: false,
        };
        let result = (|| {
            self.original.check_current()?;
            time(self.end)?;
            self.original
                .images
                .get(self.initial)
                .ok_or_else(|| refused("preparation exec transfer lost its original member"))?
                .check(self.end)?;
            self.original.check_current()?;
            time(self.end)
        })();
        if result.is_err() {
            self.quarantine();
        }
        checking.complete = result.is_ok();
        result
    }
    /// Borrow the SAME checked initial original, without reconstructing a
    /// pointer or selecting a replacement executable by pathname/status.
    pub fn initial_program(&self) -> io::Result<&Path> {
        self.check_current()?;
        let mut checking = Checking {
            revoked: &self.original.revoked,
            complete: false,
        };
        let image = &self.original.images[self.initial];
        let program = image.checked_program(self.end)?;
        checking.complete = true;
        Ok(program.program())
    }
    /// Bounded equality/restriction DATA for the existing authenticated native
    /// channel. These bytes cannot reconstruct this retained transfer owner.
    pub fn names(&self) -> io::Result<Vec<Vec<u8>>> {
        self.check_current()?;
        let names = self
            .original
            .images
            .iter()
            .map(|image| image.name().map(|name| name.to_bytes().to_vec()))
            .collect::<io::Result<Vec<_>>>()?;
        self.check_current()?;
        Ok(names)
    }
    pub fn quarantine(&self) {
        self.original.quarantine();
    }
}

/// A failed capture keeps all partial actual originals available to the
/// caller's quarantine/retirement owner. It offers no retry or owner conversion.
pub struct RefusedExecImageSet {
    retained: HeldExecImageSet,
    error: io::Error,
}
impl RefusedExecImageSet {
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    pub fn retained_image_count(&self) -> usize {
        self.retained.images.len()
    }
}
impl std::fmt::Display for RefusedExecImageSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::fmt::Debug for RefusedExecImageSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefusedExecImageSet")
            .field("error", &self.error)
            .field("retained_images", &self.retained.images.len())
            .finish()
    }
}
impl std::error::Error for RefusedExecImageSet {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

struct Checking<'a> {
    revoked: &'a AtomicBool,
    complete: bool,
}
impl Drop for Checking<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.revoked.store(true, Ordering::Release);
        }
    }
}

impl HeldExecImageSet {
    /// Portable cardinality DATA, not descriptor reservation or native support.
    pub const fn maximum_images() -> usize {
        trust_exec_policy::MAX_CLOSED_EXEC_IMAGES
    }

    /// Restrictive protocol DATA only; success creates no original owner.
    pub fn validate_preparation_names(names: &[Vec<u8>]) -> io::Result<()> {
        native_names(names).map(|_| ())
    }

    /// The authority-free receiver half of an authenticated preparation plan.
    /// On every other platform this refuses Unsupported, never an ordinary exec.
    /// Its first fixed seal is followed by scoped bootstrap/registered revocation
    /// before payload execution. Other Mach capabilities remain separate.
    ///
    /// # Safety
    /// Same trusted-entry/external-original/namespace/service requirements as
    /// the fixed native installer. Only the genuine outside owner can establish
    /// those conditions; received name DATA and this function create no owner.
    /// Bootstrap and registered send references must be disposable, as required
    /// by `revoke_current_bootstrap_and_registered_ports`; signals remain blocked. Every
    /// error is terminal, including after successful seal or partial revocation.
    pub unsafe fn install_transferred_preparation(names: &[Vec<u8>]) -> io::Result<()> {
        native_supported()?;
        let strings = native_names(names)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let borrowed = strings
                .iter()
                .map(|name| name.as_c_str())
                .collect::<Vec<_>>();
            // SAFETY: the caller supplies the actual authenticated outside
            // lifetime owner; parsing names cannot supply that authority.
            unsafe { trust_exec_policy::install_no_fork_closed_exec(&borrowed)? };
            // SAFETY: the same dedicated trusted entry owns its disposable
            // bootstrap references. Revoke only after successful first seal,
            // before the authenticated prelude ACK or any payload exec.
            unsafe { trust_exec_policy::revoke_current_bootstrap_and_registered_ports() }
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = strings;
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    /// Prepare a distinct retained host transfer for an ACTUAL original member.
    /// Initial pathname/metadata equality alone is insufficient. Multiple fixed
    /// sequential children can borrow the same held population; each native
    /// prelude separately owns its one plan, original child and first endpoint.
    pub fn prepare_transfer(
        self: &Arc<Self>,
        initial: &HeldProgram,
        deadline: Instant,
    ) -> io::Result<PreparedExecTransfer> {
        self.check_current()?;
        let mut checking = Checking {
            revoked: &self.revoked,
            complete: false,
        };
        let result = (|| {
            let end = deadline.min(self.end);
            time(end)?;
            let address = std::ptr::from_ref(initial) as usize;
            let index = self
                .images
                .iter()
                .position(|image| {
                    image
                        .binding
                        .as_ref()
                        .is_some_and(|binding| binding.address == address)
                })
                .ok_or_else(|| refused("preparation initial image is not the SAME held member"))?;
            let transfer = PreparedExecTransfer {
                original: Arc::clone(self),
                initial: index,
                end,
            };
            transfer.check_current()?;
            Ok(transfer)
        })();
        checking.complete = result.is_ok();
        result
    }
    /// Restrictive disposition only; no resource or operation is returned.
    pub fn quarantine(&self) {
        self.revoked.store(true, Ordering::Release);
    }
    pub fn capture(
        images: Vec<HeldExecImage>,
        original_deadline: Instant,
    ) -> Result<Self, RefusedExecImageSet> {
        let end = images
            .iter()
            .fold(original_deadline, |end, image| end.min(image.end));
        let mut retained = Self {
            images,
            credentials: None,
            creator: std::process::id(),
            end,
            revoked: AtomicBool::new(false),
            attempted: AtomicBool::new(false),
        };
        let result = (|| {
            native_supported()?;
            time(end)?;
            if retained.images.is_empty()
                || retained.images.len() > trust_exec_policy::MAX_CLOSED_EXEC_IMAGES
            {
                return Err(refused("held exec image population exceeds bound"));
            }
            retained.credentials = Some(command_search::credentials(end)?);
            for image in &mut retained.images {
                image.capture(end)?;
            }
            for (index, image) in retained.images.iter().enumerate() {
                let name = image.name()?;
                if retained.images[..index]
                    .iter()
                    .any(|other| other.name().is_ok_and(|other| other == name))
                {
                    return Err(refused("held exec image names are not distinct"));
                }
            }
            retained.check_current()
        })();
        match result {
            Ok(()) => Ok(retained),
            Err(error) => {
                retained.revoked.store(true, Ordering::Release);
                Err(RefusedExecImageSet { retained, error })
            }
        }
    }
    pub fn original_deadline(&self) -> Instant {
        self.end
    }
    pub fn check_current(&self) -> io::Result<()> {
        // Do not enter a foreign parent's locks/projectors after fork. The
        // foreign process cannot use its copied Rust representation as custody.
        if self.creator != std::process::id() {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        if self.revoked.load(Ordering::Acquire) {
            return Err(refused("held exec image set is quarantined"));
        }
        let mut checking = Checking {
            revoked: &self.revoked,
            complete: false,
        };
        let result = (|| {
            native_supported()?;
            time(self.end)?;
            let expected = self
                .credentials
                .as_ref()
                .ok_or_else(|| refused("exec inputs have no original credentials"))?;
            if *expected != command_search::credentials(self.end)? {
                return Err(refused("held exec image credentials changed"));
            }
            for image in &self.images {
                image.check(self.end)?;
            }
            if *expected != command_search::credentials(self.end)? {
                return Err(refused(
                    "held exec image credentials changed during validation",
                ));
            }
            time(self.end)?;
            if self.revoked.load(Ordering::Acquire) {
                return Err(refused("held exec image set is quarantined"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        checking.complete = result.is_ok();
        result
    }

    /// Install the one fixed policy on this process, once before payload exec.
    /// Any failure permanently quarantines this set; it cannot justify retry.
    ///
    /// # Safety
    /// In addition to `install_no_fork_closed_exec`'s trusted-entry contract, a
    /// SAME external lifetime owner must retain these actual original inputs
    /// through all execs and final child retirement. This self restriction does
    /// not transfer this Rust guard to that owner: exec destroys local guards.
    /// The same disposable-bootstrap and terminal-failure requirements as
    /// `install_transferred_preparation` apply; signals remain blocked.
    /// No complete-chain, GO, NativeClosed, source, Seed or SDK result exists.
    pub unsafe fn install_current(&self) -> io::Result<()> {
        self.check_current()?;
        let mut checking = Checking {
            revoked: &self.revoked,
            complete: false,
        };
        if self
            .attempted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.revoked.store(true, Ordering::Release);
            return Err(refused("held exec policy already attempted"));
        }
        let result = (|| {
            let names = self
                .images
                .iter()
                .map(|image| image.name().map(|name| name.to_bytes().to_vec()))
                .collect::<io::Result<Vec<_>>>()?;
            // SAFETY: the surrounding unsafe contract supplies the actual
            // external lifetime/namespace owner. Names come ONLY from the
            // retained checked projections, with unchanged original endpoint.
            unsafe {
                Self::install_transferred_preparation(&names)?;
            }
            self.check_current()
        })();
        if result.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        checking.complete = result.is_ok();
        result
    }
}

#[cfg(test)]
#[path = "closed_exec_tests.rs"]
mod tests;
