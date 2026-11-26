use super::*;

#[test]
fn cannot_seal_a_prepared_tree_outside_the_install() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("version");
    let source = temp.path().join("prepared");
    fs::create_dir_all(root.join(".local")).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("sunrise-resources.json"), "{}").unwrap();
    for source in [source, root.join("../prepared")] {
        assert!(Resources::seal(&root, &source, None, |_| Ok(None)).is_err());
    }
    assert!(!parent(&root).exists());
}

#[test]
fn missing_snapshot_uses_the_same_seal_path_as_first_preparation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join(".local/prepared");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("sunrise-resources.json"), "{}").unwrap();
    let prepared = Resources::seal(root, &source, None, |_| Ok(None)).unwrap();
    let path = prepared.path_at(root);
    for missing in [&path, path.parent().unwrap()] {
        fs::remove_dir_all(missing).unwrap();
        assert_eq!(Resources::current(root).unwrap(), None);
        assert_eq!(
            Resources::seal(root, &source, None, |_| Ok(None)).unwrap(),
            prepared
        );
        assert_eq!(Resources::current(root).unwrap().as_ref(), Some(&prepared));
    }
}

#[test]
fn a_receipt_cannot_select_stale_or_server_preparation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join(".local/prepared");
    fs::create_dir_all(&source).unwrap();
    let lib = root.join("crates/sunrise-prepare/src/lib.rs");
    fs::create_dir_all(lib.parent().unwrap()).unwrap();
    fs::write(lib, "pub const OUTPUT_VERSION: u32 = 8;\n").unwrap();
    let manifest = source.join("sunrise-resources.json");
    let stamp = source.join("sunrise-prepare.json");
    fs::write(&manifest, "{}").unwrap();
    fs::write(&stamp, r#"{"output_version":7}"#).unwrap();
    Resources::seal(root, &source, None, |_| Ok(None)).unwrap();
    assert_eq!(Resources::current(root).unwrap(), None);

    fs::write(&stamp, r#"{"output_version":8}"#).unwrap();
    fs::write(&manifest, r#"{"profile":"server"}"#).unwrap();
    Resources::seal(root, &source, None, |_| Ok(None)).unwrap();
    assert_eq!(Resources::current(root).unwrap(), None);

    fs::write(&manifest, "{}").unwrap();
    let full = Resources::seal(root, &source, None, |_| Ok(None)).unwrap();
    assert_eq!(Resources::current(root).unwrap(), Some(full));
}

#[test]
fn prepared_snapshot_does_not_read_a_cache_from_a_different_tree() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let source = root.join(".local/prepared");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("sunrise-resources.json"), "{}").unwrap();
    fs::write(source.join("blocks.json"), "{\"old\": true}").unwrap();
    fs::write(source.join(preparer::DATA_FILE), b"untrusted").unwrap();
    let prepared = Resources::seal(root, &source, None, |dir| {
        let data = dir.join(preparer::DATA_FILE);
        assert!(!data.exists());
        fs::write(&data, b"generated from the snapshot").unwrap();
        Ok(digest_file(&data).unwrap())
    })
    .unwrap();
    let snapshot = parent(root).join(key(prepared.primary)).join("resources");
    assert_eq!(
        fs::read_to_string(snapshot.join("blocks.json")).unwrap(),
        "{\"old\": true}"
    );
    assert_eq!(
        Resources::seal(root, &source, None, |_| panic!("snapshot reused")).unwrap(),
        prepared
    );
    fs::write(source.join("blocks.json"), "{\"new\": true}").unwrap();
    assert_eq!(Resources::current(root).unwrap().as_ref(), Some(&prepared));
    fs::write(receipt(root).unwrap(), b"partial").unwrap();
    assert_eq!(Resources::current(root).unwrap(), None);
    Resources::invalidate(root).unwrap();
    assert_eq!(Resources::current(root).unwrap(), None);
    let changed = Resources::seal(root, &source, None, |_| Ok(None)).unwrap();
    assert_ne!(prepared.primary, changed.primary);
    assert_eq!(Resources::current(root).unwrap(), Some(changed));
    fs::write(snapshot.join(preparer::DATA_FILE), b"different cache").unwrap();
    fs::write(snapshot.join("sunrise-data.bin.123.tmp"), b"pending cache").unwrap();
    let primary = || artifact::tree(&snapshot, true).unwrap();
    assert_eq!(primary(), Some(prepared.primary));
    fs::write(snapshot.join("blocks.json"), b"different primary bytes").unwrap();
    assert_ne!(primary(), Some(prepared.primary));
}

fn nested_source(root: &Path) -> PathBuf {
    let source = root.join(".local/prepared");
    fs::create_dir_all(source.join("assets/empty")).unwrap();
    fs::create_dir_all(source.join("assets/deep/er")).unwrap();
    fs::write(source.join("sunrise-resources.json"), "{}").unwrap();
    fs::write(source.join("assets/deep/er/blob.bin"), vec![7u8; 150_000]).unwrap();
    fs::write(source.join("assets/empty.txt"), b"").unwrap();
    fs::write(source.join(preparer::DATA_FILE), b"untrusted").unwrap();
    source
}

#[test]
fn one_and_two_workers_seal_the_same_snapshot() {
    let mut sealed = Vec::new();
    for workers in [1, 2] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let source = nested_source(root);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = source.join("assets/empty.txt");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let prepared = Resources::seal_with(root, &source, workers, None, |_| Ok(None)).unwrap();
        let snapshot = prepared.path_at(root);
        assert_eq!(
            artifact::tree(&snapshot, true).unwrap(),
            Some(prepared.primary)
        );
        assert!(snapshot.join("assets/empty").is_dir());
        assert!(!snapshot.join(preparer::DATA_FILE).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(snapshot.join("assets/empty.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        assert_eq!(Resources::current(root).unwrap().as_ref(), Some(&prepared));
        sealed.push(prepared);
    }
    assert_eq!(sealed[0], sealed[1]);
}

#[test]
fn a_stopped_seal_returns_the_cancellation_and_leaves_no_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = nested_source(root);
    let stop = Cancellation::default();
    stop.stop();
    for workers in [1, 2] {
        let error = Resources::seal_with(root, &source, workers, Some(&stop), |_| {
            panic!("sealed after a stop")
        })
        .unwrap_err();
        assert_eq!(Some(error), stop.check().err());
        assert!(!parent(root).exists());
    }
    let copying = copy_tree_with(&source, &root.join("copy"), true, 0, 2, Some(&stop), None);
    assert!(copying.is_err());
    let leftovers: Vec<_> = fs::read_dir(root.join(".local"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, ["prepared"]);
}

#[cfg(unix)]
#[test]
fn a_link_found_while_copying_is_refused_and_every_worker_is_joined() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = nested_source(root);
    let link = source.join("assets/deep/link");
    std::os::unix::fs::symlink(source.join("assets/empty.txt"), &link).unwrap();
    for workers in [1, 2] {
        let error = copy_tree_with(&source, &root.join("copy"), true, 0, workers, None, None)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), setup_error(&link));
        fs::remove_dir_all(root.join("copy")).unwrap();
        let error = Resources::seal_with(root, &source, workers, None, |_| Ok(None)).unwrap_err();
        assert_eq!(error, at(&link, io::ErrorKind::InvalidData.into()));
        assert!(!parent(root).exists());
    }
}

#[test]
fn a_home_sealed_by_v1_3_4_is_selected_and_kept_without_sealing_again() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = nested_source(root);
    // v1.3.4's layout: `<key>/resources` beside `current.json`, written by hand here.
    let primary = artifact::tree(&source, true).unwrap().unwrap();
    let sealed = Resources {
        primary,
        accelerator: None,
    };
    let snapshot = sealed.path_at(root);
    fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    copy_tree_with(&source, &snapshot, true, 0, 1, None, None).unwrap();
    write_receipt(&parent(root).join(CURRENT), &sealed).unwrap();
    assert_eq!(Resources::current(root).unwrap().as_ref(), Some(&sealed));
    assert_eq!(artifact::tree(&snapshot, true).unwrap(), Some(primary));
    let again = Resources::seal(root, &source, None, |_| panic!("sealed again")).unwrap();
    assert_eq!(again, sealed);
}
