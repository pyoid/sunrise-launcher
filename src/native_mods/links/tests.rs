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
