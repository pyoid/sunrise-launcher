use super::*;

#[test]
fn a_claimed_folder_is_not_gone_while_this_process_lives() {
    let dir = tempfile::tempdir().unwrap();
    assert!(gone(dir.path()), "no .owner: a launcher before 1.3.5");
    claim(dir.path()).unwrap();
    assert!(!gone(dir.path()));
    assert!(claim(dir.path()).is_err(), "claimed once");
}

#[cfg(unix)]
#[test]
fn a_linked_owner_file_is_never_gone() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("elsewhere");
    fs::write(&elsewhere, b"").unwrap();
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    std::os::unix::fs::symlink(&elsewhere, folder.join(FILE)).unwrap();
    assert!(!gone(&folder));
}
