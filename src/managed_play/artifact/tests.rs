use super::*;

#[test]
fn image_covers_library_native_payload_and_architecture_but_not_logs() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    fs::write(dir.join(backend::CLIENT), b"game").unwrap();
    fs::write(dir.join(".installed"), []).unwrap();
    fs::create_dir(dir.join("native-data")).unwrap();
    fs::create_dir_all(dir.join("deps/nested")).unwrap();
    let paths = [
        "native.dll",
        "deps/nested/libsdk.so",
        "native-data/data.json",
        backend::GAME_BUILD_ARCHITECTURE,
    ];
    for path in paths {
        fs::write(dir.join(path), b"original").unwrap();
    }
    let capture = || Artifact::capture_at("tag", dir).unwrap();
    let sealed = capture();
    fs::write(dir.join("latest.log"), b"mutable game log").unwrap();
    assert_eq!(sealed, capture());
    for path in paths {
        fs::write(dir.join(path), b"different").unwrap();
        assert_ne!(sealed, capture(), "{path}");
        fs::write(dir.join(path), b"original").unwrap();
        assert_eq!(sealed, capture(), "{path}");
    }
    fs::write(dir.join("extra.so.1"), b"extra library").unwrap();
    assert_ne!(sealed, capture());
}

/// The digest v1.3.4's serial `artifact::tree` gave this fixture, computed once from that
/// algorithm and committed so no later change can move it.
const GOLDEN: &str = "5d3eb036328857245589db22a45054920121f1b26a9690ff94b2759ac8189d07";
#[cfg(target_os = "linux")]
const GOLDEN_CASE: &str = "1a08ece88d725eca4c7b167f093c102a908c1e542dde2e9850d1089d2ad46af2";

fn hex(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn fixture(dir: &Path) {
    for sub in ["empty-dir", "nested/deep/deeper", "nested/a/b"] {
        fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    for (path, bytes) in [
        ("Alpha.txt", &b"alpha-data"[..]),
        ("Zebra.txt", b"zebra"),
        ("beta.txt", b""),
        ("caf\u{e9}.txt", b"latte"),
        ("nested/deep/deeper/leaf.bin", &big),
        ("nested/empty.txt", b""),
        ("nested/a/b/file.txt", b"x"),
    ] {
        fs::write(dir.join(path), bytes).unwrap();
    }
}

#[test]
fn tree_digest_is_the_serial_v1_3_4_digest_for_any_worker_count() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    for workers in [1, 2, 5] {
        let (digest, files) = tree_with_workers(temp.path(), false, workers, None)
            .unwrap()
            .unwrap();
        assert_eq!(hex(digest), GOLDEN, "{workers} workers");
        assert_eq!(files, 7);
    }
    assert_eq!(hex(tree(temp.path(), false).unwrap().unwrap()), GOLDEN);
}

#[cfg(target_os = "linux")]
#[test]
fn names_differing_only_in_case_keep_their_byte_order() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("Same.txt"), b"upper").unwrap();
    fs::write(temp.path().join("same.txt"), b"lower").unwrap();
    for workers in [1, 2] {
        let (digest, _) = tree_with_workers(temp.path(), false, workers, None)
            .unwrap()
            .unwrap();
        assert_eq!(hex(digest), GOLDEN_CASE);
    }
}

#[test]
fn the_generated_cache_is_skipped_only_at_the_top_when_asked() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    fs::write(temp.path().join("sunrise-data.bin"), b"cache").unwrap();
    fs::write(temp.path().join("sunrise-data.bin.42.tmp"), b"cache").unwrap();
    assert_eq!(hex(tree(temp.path(), true).unwrap().unwrap()), GOLDEN);
    assert_ne!(hex(tree(temp.path(), false).unwrap().unwrap()), GOLDEN);
    fs::write(temp.path().join("nested/sunrise-data.bin"), b"cache").unwrap();
    assert_ne!(hex(tree(temp.path(), true).unwrap().unwrap()), GOLDEN);
}

#[test]
fn a_missing_root_is_none_and_too_deep_a_tree_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(tree(&temp.path().join("absent"), false), Ok(None));
    let mut deep = temp.path().to_path_buf();
    for _ in 0..66 {
        deep.push("d");
    }
    fs::create_dir_all(&deep).unwrap();
    for workers in [1, 2] {
        assert!(tree_with_workers(temp.path(), false, workers, None).is_err());
    }
}

#[test]
fn a_stopped_attempt_hashes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let stop = Cancellation::default();
    stop.stop();
    let error = tree_with_workers(temp.path(), false, 2, Some(&stop)).unwrap_err();
    assert_eq!(Some(error.to_string()), stop.check().err());
}

#[cfg(unix)]
#[test]
fn a_symlink_in_the_middle_of_the_tree_is_refused_by_every_worker_count() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let link = temp.path().join("nested/deep/link.txt");
    std::os::unix::fs::symlink(temp.path().join("Alpha.txt"), &link).unwrap();
    let expected = at(&link, io::ErrorKind::InvalidData.into());
    for workers in [1, 2] {
        assert_eq!(
            tree_with_workers(temp.path(), false, workers, None),
            Err(expected.clone())
        );
    }
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_in_the_middle_of_the_tree_is_an_error_not_a_hang() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let locked = temp.path().join("nested/a/b/file.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o0)).unwrap();
    if fs::File::open(&locked).is_ok() {
        return; // Running as a user the mode does not bind.
    }
    for workers in [1, 2] {
        let error = tree_with_workers(temp.path(), false, workers, None).unwrap_err();
        assert!(
            error.to_string().starts_with(&locked.display().to_string()),
            "{error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_hard_linked_file_is_hashed_as_it_was_before_the_pool() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let before = tree(temp.path(), false).unwrap().unwrap();
    fs::remove_file(temp.path().join("nested/empty.txt")).unwrap();
    fs::hard_link(
        temp.path().join("beta.txt"),
        temp.path().join("nested/empty.txt"),
    )
    .unwrap();
    assert_eq!(tree(temp.path(), false).unwrap().unwrap(), before);
}
