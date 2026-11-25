use super::*;

#[test]
fn a_placed_file_is_a_new_link_and_never_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("from");
    let to = dir.path().join("to");
    fs::write(&from, b"source").unwrap();
    fs::write(&to, b"older").unwrap();
    // Whatever was at `to` is unlinked first: the other name for it keeps its bytes.
    let other = dir.path().join("other");
    fs::hard_link(&to, &other).unwrap();
    assert_eq!(link_or_copy(&from, &to).unwrap(), Placed::Linked);
    assert_eq!(fs::read(&to).unwrap(), b"source");
    assert_eq!(fs::read(&other).unwrap(), b"older");
    assert!(link_or_copy(dir.path(), &dir.path().join("x")).is_err());
}

/// A game build links the read-only SDK package's files: publishing flushes the tree it is in,
/// and a later build replaces the link. Windows refuses both for a read-only file as it is.
#[test]
fn a_read_only_link_can_be_flushed_and_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("runtime.dll");
    fs::write(&package, b"runtime").unwrap();
    let mut permissions = fs::metadata(&package).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&package, permissions).unwrap();
    let build = dir.path().join("build");
    fs::create_dir(&build).unwrap();
    let installed = build.join("runtime.dll");
    link_or_copy(&package, &installed).unwrap();
    crate::profile_txn::durable::flush_tree(&build).unwrap();
    let newer = dir.path().join("newer.dll");
    fs::write(&newer, b"newer").unwrap();
    link_or_copy(&newer, &installed).unwrap();
    assert_eq!(fs::read(&installed).unwrap(), b"newer");
    assert_eq!(fs::read(&package).unwrap(), b"runtime");
}
