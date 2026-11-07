use super::*;

#[test]
fn registered_refresh_never_replaces_lifetime_or_parent() {
    let mut birth = Process::current().unwrap();
    birth.identity.birth ^= 1;
    assert!(birth.refresh_registered_member().is_err());
    let mut parent = Process::current().unwrap();
    parent.identity.parent ^= 1;
    assert!(parent.refresh_registered_member().is_err());
    #[cfg(target_os = "macos")]
    {
        let mut parent_birth = Process::current().unwrap();
        parent_birth.identity.parent_birth ^= 1;
        assert!(parent_birth.refresh_registered_member().is_err());
    }
}
