//! Portable type/DATA boundary; no fabricated native program or live Root.
use super::*;
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
use std::{sync::atomic::AtomicUsize, time::Duration};

#[test]
fn portable_name_bounds_are_data_and_not_native_admission() {
    assert!(HeldExecImageSet::maximum_images() > 0);
    assert!(HeldExecImageSet::validate_preparation_names(&[b"/original/program".to_vec()]).is_ok());
    for names in [
        Vec::new(),
        vec![b"relative".to_vec()],
        vec![b"/original/../alias".to_vec()],
        vec![b"/original/program".to_vec(); HeldExecImageSet::maximum_images() + 1],
    ] {
        assert!(HeldExecImageSet::validate_preparation_names(&names).is_err());
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn unsupported_native_capture_keeps_partial_originals_without_a_program_constructor() {
    struct Partial(Arc<AtomicUsize>);
    impl Drop for Partial {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn no_program(_: &Partial, _: Instant) -> io::Result<&HeldProgram> {
        Err(io::ErrorKind::Unsupported.into())
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let end = Instant::now() + Duration::from_secs(60);
    let partial = HeldExecImage::retain(Box::new(Partial(Arc::clone(&drops))), end, no_program);
    let refused = match HeldExecImageSet::capture(vec![partial], end) {
        Err(refused) => refused,
        Ok(_) => panic!("unsupported cell constructed a native exec owner"),
    };
    assert_eq!(refused.error().kind(), io::ErrorKind::Unsupported);
    assert_eq!(refused.retained_image_count(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(
        unsafe {
            HeldExecImageSet::install_transferred_preparation(&[b"/original/program".to_vec()])
        }
        .unwrap_err()
        .kind(),
        io::ErrorKind::Unsupported
    );
    drop(refused);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
