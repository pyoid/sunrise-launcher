use super::*;
use crate::native_mods::tool::{Built, CheckRoute, LibraryKind, LibraryRecord};

const FOLDER_NAME: &str = "api-7cfdf81463eb-0123456789abcdef";

fn build() -> Build<'static> {
    Build {
        sdk: "1111111111111111111111111111111111111111111111111111111111111111",
        tool: "2222222222222222222222222222222222222222222222222222222222222222",
        route: "prebuilt-api",
        revision: "7cfdf81463ebb4a19705768273909f7f3f03601d",
        mixins: "e59da9094f0c2fdda6c627fe9eff22dec6a906f0",
        staging: "area-aware-mixins-input-v1",
        profile: "release",
        rust: "1.99.1",
        rust_target: "x86_64-pc-windows-gnu",
        platform: "",
        target: "client",
        mixins_from_source: false,
    }
}

/// ae2 and hnn on their own, and ftb linking to ae2, in dependency order.
fn mods(files: &[(&str, &str)]) -> Vec<Mod<'static>> {
    let file = |id: &str| {
        files
            .iter()
            .find(|(m, _)| *m == id)
            .map_or_else(|| format!("{id}-files"), |(_, f)| (*f).to_owned())
    };
    vec![
        Mod {
            id: "ae2",
            files: file("ae2"),
            links: vec![],
        },
        Mod {
            id: "ftb",
            files: file("ftb"),
            links: vec!["ae2"],
        },
        Mod {
            id: "hnn",
            files: file("hnn"),
            links: vec![],
        },
    ]
}

/// A work folder as the tool leaves it after an API-route build of `mods` (id and library
/// bytes), with the launcher's verified libraries and a patch library.
fn work_with(root: &Path, mods: &[(&str, &[u8])]) -> (PathBuf, Built, Vec<Library>) {
    let work = root.join("work");
    let folder = work.join("prebuilt-build").join(FOLDER_NAME);
    fs::create_dir_all(folder.join("staged/ae2/src")).unwrap();
    fs::write(folder.join("staged/ae2/src/lib.rs"), b"fn main() {}").unwrap();
    let mut libraries = Vec::new();
    let mut records = Vec::new();
    let mut paths = Vec::new();
    let mut add = |id: &str, bytes: &[u8], kind: LibraryKind| {
        let file = format!("mod_{id}.dll");
        let path = folder.join(&file);
        fs::write(&path, bytes).unwrap();
        fs::write(folder.join(format!("mod_{id}.fingerprint")), id.as_bytes()).unwrap();
        fs::write(folder.join(format!("mod_{id}.identity.json")), b"{}").unwrap();
        if cfg!(windows) {
            fs::write(folder.join(format!("lib{file}.a")), b"import").unwrap();
        }
        let sha256 = sha256_file(&path).unwrap();
        libraries.push(Library {
            name: file.clone(),
            source: path.clone(),
            sha256: sha256.clone(),
            shared: false,
        });
        records.push(LibraryRecord {
            sha256,
            file,
            kind,
            mod_id: id.to_owned(),
            identity: String::new(),
        });
        paths.push(path);
    };
    for (id, bytes) in mods {
        add(id, bytes, LibraryKind::Mod);
    }
    add("sunrise_patch", b"patch", LibraryKind::Patch);
    let built = Built {
        route: CheckRoute::PrebuiltApi,
        binary: work.join("package/runtime/bin/sunrise-client.exe"),
        libraries: paths,
        mods: mods.iter().map(|(id, _)| (*id).to_owned()).collect(),
        records,
        internals: Default::default(),
    };
    (work, built, libraries)
}

const ALL: &[(&str, &[u8])] = &[("ae2", b"ae2"), ("ftb", b"ftb"), ("hnn", b"hnn")];

fn folder(work: &Path) -> PathBuf {
    work.join("prebuilt-build").join(FOLDER_NAME)
}

fn sidecars_per_mod() -> usize {
    if cfg!(windows) {
        3
    } else {
        2
    }
}

#[test]
fn an_unchanged_mod_gets_back_its_verified_library() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(&dir.path().join("first"), ALL);
    assert_eq!(
        kept.save(&work, &built, &libraries, &list, &keys).unwrap(),
        3
    );
    let next = dir.path().join("second/work");
    assert_eq!(kept.seed(&next, &list, &keys), ["ae2", "ftb", "hnn"]);
    for id in ["ae2", "ftb", "hnn"] {
        let name = format!("mod_{id}.dll");
        assert_eq!(
            fs::read(folder(&next).join(&name)).unwrap(),
            fs::read(folder(&work).join(&name)).unwrap()
        );
        assert!(folder(&next)
            .join(format!("mod_{id}.fingerprint"))
            .is_file());
        // The tool stamps its identity again; only what it checks is kept.
        assert!(!folder(&next)
            .join(format!("mod_{id}.identity.json"))
            .exists());
    }
    let files = fs::read_dir(folder(&next)).unwrap().count();
    assert_eq!(files, 3 * (sidecars_per_mod()));
    // The patch library is built from the whole set: never kept.
    assert!(!folder(&next).join("mod_sunrise_patch.dll").exists());
    assert!(!folder(&next).join("staged").exists());
}

#[test]
fn an_empty_store_seeds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let work = dir.path().join("work");
    assert!(Kept::new(dir.path()).seed(&work, &list, &keys).is_empty());
    assert!(!work.exists());
}

#[test]
fn any_change_to_the_build_misses_every_mod() {
    let changes: [fn(&mut Build<'static>); 12] = [
        |b| b.sdk = "3333333333333333333333333333333333333333333333333333333333333333",
        |b| b.tool = "4444444444444444444444444444444444444444444444444444444444444444",
        |b| b.route = "prebuilt",
        |b| b.revision = "9425651",
        |b| b.mixins = "other",
        |b| b.staging = "other",
        |b| b.profile = "debug",
        |b| b.rust = "1.100.0",
        |b| b.rust_target = "x86_64-unknown-linux-gnu",
        |b| b.platform = "arm64",
        |b| b.target = "server",
        |b| b.mixins_from_source = true,
    ];
    let list = mods(&[]);
    let before = keys(&build(), &list);
    for (n, change) in changes.iter().enumerate() {
        let mut other = build();
        change(&mut other);
        let after = keys(&other, &list);
        for id in ["ae2", "ftb", "hnn"] {
            assert_ne!(before[id], after[id], "change {n}, {id}");
        }
    }
    // Parts are length-prefixed: moving text between two of them is another key.
    let mut shifted = build();
    shifted.target = "clientx";
    shifted.platform = "";
    let mut other = build();
    other.target = "client";
    other.platform = "x";
    assert_ne!(keys(&shifted, &list)["ae2"], keys(&other, &list)["ae2"]);
}

#[test]
fn a_changed_mod_misses_and_so_does_every_mod_linking_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let old = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    kept.save(&work, &built, &libraries, &list, &old).unwrap();
    let changed = mods(&[("ae2", "ae2-files-v2")]);
    let new = keys(&build(), &changed);
    assert_ne!(old["ae2"], new["ae2"]);
    assert_ne!(old["ftb"], new["ftb"]);
    assert_eq!(old["hnn"], new["hnn"]);
    let next = dir.path().join("next");
    assert_eq!(kept.seed(&next, &changed, &new), ["hnn"]);
    // Only ftb changed: ae2 and hnn are reused.
    let changed = mods(&[("ftb", "ftb-files-v2")]);
    let new = keys(&build(), &changed);
    let next = dir.path().join("again");
    assert_eq!(kept.seed(&next, &changed, &new), ["ae2", "hnn"]);
    assert!(!folder(&next).join("mod_ftb.dll").exists());
}

#[test]
fn a_mod_is_rebuilt_once_a_library_it_links_to_was_rebuilt() {
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    // ae2 rebuilt with other bytes, or the same bytes under another build identity, and kept
    // under the same key by a build that didn't build ftb.
    let rebuilds: [(&[u8], &str); 2] = [(b"ae2 rebuilt", ""), (b"ae2", "another identity")];
    for (n, (bytes, identity)) in rebuilds.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let kept = Kept::new(&dir.path().join("native"));
        let (work, built, libraries) = work_with(&dir.path().join("first"), ALL);
        kept.save(&work, &built, &libraries, &list, &keys).unwrap();
        let (work, mut built, libraries) = work_with(&dir.path().join("second"), &[("ae2", bytes)]);
        built.records[0].identity = identity.to_owned();
        assert_eq!(
            kept.save(&work, &built, &libraries, &list, &keys).unwrap(),
            1
        );
        let next = dir.path().join("next");
        assert_eq!(
            kept.seed(&next, &list, &keys),
            ["ae2", "hnn"],
            "rebuild {n}"
        );
        assert_eq!(fs::read(folder(&next).join("mod_ae2.dll")).unwrap(), bytes);
        assert!(!folder(&next).join("mod_ftb.dll").exists());
    }
}

#[test]
fn a_mod_whose_link_isnt_reused_is_not_reused_either() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    kept.save(&work, &built, &libraries, &list, &keys).unwrap();
    fs::remove_file(kept.index_path(&keys["ae2"])).unwrap();
    assert_eq!(kept.seed(&dir.path().join("next"), &list, &keys), ["hnn"]);
}

#[test]
fn a_corrupted_library_is_a_miss_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    kept.save(&work, &built, &libraries, &list, &keys).unwrap();
    let hnn = libraries.iter().find(|l| l.name == "mod_hnn.dll").unwrap();
    fs::write(kept.blobs().join(&hnn.sha256), b"not hnn").unwrap();
    let next = dir.path().join("next");
    assert_eq!(kept.seed(&next, &list, &keys), ["ae2", "ftb"]);
    // No part of hnn's entry is left for the tool to trust.
    assert!(!folder(&next).join("mod_hnn.dll").exists());
    assert!(!folder(&next).join("mod_hnn.fingerprint").exists());
}

#[test]
fn a_missing_or_linked_blob_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    kept.save(&work, &built, &libraries, &list, &keys).unwrap();
    let ae2 = libraries.iter().find(|l| l.name == "mod_ae2.dll").unwrap();
    fs::remove_file(kept.blobs().join(&ae2.sha256)).unwrap();
    let next = dir.path().join("next");
    // ftb links to ae2, so it goes too.
    assert_eq!(kept.seed(&next, &list, &keys), ["hnn"]);
    assert!(!folder(&next).join("mod_ae2.fingerprint").exists());
}

#[test]
fn an_entry_naming_paths_outside_its_folder_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    kept.save(&work, &built, &libraries, &list, &keys).unwrap();
    let good: Index =
        serde_json::from_slice(&fs::read(kept.index_path(&keys["ae2"])).unwrap()).unwrap();
    let sha = good.files.values().next().unwrap().clone();
    for (n, (folder, name)) in [
        ("..", "mod_ae2.dll"),
        ("target", "mod_ae2.dll"),
        (FOLDER_NAME, "../mod_ae2.dll"),
        (FOLDER_NAME, "..\\mod_ae2.dll"),
        (FOLDER_NAME, "sub/mod_ae2.dll"),
    ]
    .into_iter()
    .enumerate()
    {
        let bad = Index {
            format: FORMAT.into(),
            folder: folder.into(),
            library: name.to_owned(),
            identity: String::new(),
            files: [(name.to_owned(), sha.clone())].into(),
            links: BTreeMap::new(),
        };
        fs::write(
            kept.index_path(&keys["ae2"]),
            serde_json::to_vec(&bad).unwrap(),
        )
        .unwrap();
        let next = dir.path().join(format!("next-{n}"));
        assert_eq!(kept.seed(&next, &list, &keys), ["hnn"], "{folder} {name}");
        assert!(!dir.path().join("mod_ae2.dll").exists());
        assert!(!next.join("mod_ae2.dll").exists());
    }
}

#[test]
fn registry_route_libraries_are_never_kept() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, mut built, mut libraries) = work_with(dir.path(), ALL);
    // The registry route's stamped libraries are in a Cargo target folder below its own.
    let release =
        work.join("prebuilt-build/7cfdf81463eb-0123/target/x86_64-pc-windows-gnu/release");
    fs::create_dir_all(&release).unwrap();
    for library in &mut libraries {
        let moved = release.join(&library.name);
        fs::copy(&library.source, &moved).unwrap();
        library.source = moved;
    }
    built.route = CheckRoute::Prebuilt;
    assert_eq!(
        kept.save(&work, &built, &libraries, &list, &keys).unwrap(),
        0
    );
    assert!(kept.seed(&dir.path().join("next"), &list, &keys).is_empty());
}

#[test]
fn a_library_whose_bytes_changed_after_verification_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    fs::write(folder(&work).join("mod_ae2.dll"), b"swapped").unwrap();
    let swapped = sha256_file(&folder(&work).join("mod_ae2.dll")).unwrap();
    assert!(kept.save(&work, &built, &libraries, &list, &keys).is_err());
    assert!(!kept.index_path(&keys["ae2"]).exists());
    // The unverified bytes never reach the store.
    assert!(!kept.blobs().join(swapped).exists());
}

#[test]
fn a_new_build_replaces_the_entry_and_unused_blobs_go() {
    let dir = tempfile::tempdir().unwrap();
    let mut kept = Kept::new(&dir.path().join("native"));
    kept.grace = std::time::Duration::ZERO;
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(&dir.path().join("a"), &[("ae2", b"ae2 old")]);
    kept.save(&work, &built, &libraries, &list, &keys).unwrap();
    let (work, built, libraries) = work_with(&dir.path().join("b"), &[("ae2", b"ae2 new")]);
    kept.save(&work, &built, &libraries, &list, &keys).unwrap();
    let next = dir.path().join("next");
    assert_eq!(kept.seed(&next, &list[..1], &keys), ["ae2"]);
    assert_eq!(
        fs::read(folder(&next).join("mod_ae2.dll")).unwrap(),
        b"ae2 new"
    );
    // The old library's blob is named by no entry and is past the grace, so it was removed.
    let blobs: BTreeSet<_> = fs::read_dir(kept.blobs())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let index: Index =
        serde_json::from_slice(&fs::read(kept.index_path(&keys["ae2"])).unwrap()).unwrap();
    assert_eq!(blobs, index.files.into_values().collect());
}

#[test]
fn only_the_most_recently_used_entries_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let (work, built, libraries) = work_with(dir.path(), &[("ae2", b"ae2")]);
    let list = &mods(&[])[..1];
    let key =
        |n: usize| -> BTreeMap<String, String> { [("ae2".to_owned(), format!("{n:064x}"))].into() };
    for n in 0..ENTRIES + 2 {
        kept.save(&work, &built, &libraries, list, &key(n)).unwrap();
        // Distinct modified times, oldest first.
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(10_000 - n as u64 * 60);
        fs::File::options()
            .write(true)
            .open(kept.index_path(&key(n)["ae2"]))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    kept.save(&work, &built, &libraries, list, &key(ENTRIES + 2))
        .unwrap();
    let present: Vec<bool> = (0..ENTRIES + 3)
        .map(|n| kept.index_path(&key(n)["ae2"]).exists())
        .collect();
    assert_eq!(present.iter().filter(|p| !**p).count(), 3);
    assert!(!present[0] && !present[1] && !present[2]);
}

#[test]
fn concurrent_attempts_only_ever_seed_whole_verified_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("native");
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let versions: Vec<Vec<u8>> = (0..4).map(|n| vec![n as u8; 64 << 10]).collect();
    std::thread::scope(|scope| {
        for (n, bytes) in versions.iter().enumerate() {
            let (root, keys, list) = (&root, &keys, &list);
            let base = dir.path().join(format!("writer-{n}"));
            scope.spawn(move || {
                for round in 0..8 {
                    let (work, built, libraries) = work_with(
                        &base.join(round.to_string()),
                        &[("ae2", bytes), ("ftb", b"ftb"), ("hnn", b"hnn")],
                    );
                    let saved = Kept::new(root).save(&work, &built, &libraries, list, keys);
                    // On Windows a blob another attempt is copying out can't be replaced: that
                    // save fails, costing only a later miss.
                    assert!(saved.is_ok() || cfg!(windows), "{saved:?}");
                }
            });
        }
        for reader in 0..4 {
            let (root, keys, list, versions) = (&root, &keys, &list, &versions);
            let base = dir.path().join(format!("reader-{reader}"));
            scope.spawn(move || {
                for round in 0..16 {
                    let next = base.join(round.to_string());
                    // A race may cost a library (a miss), never give a mixed one.
                    let seeded = Kept::new(root).seed(&next, list, keys);
                    if seeded.iter().any(|id| id == "ae2") {
                        let ae2 = fs::read(folder(&next).join("mod_ae2.dll")).unwrap();
                        assert!(versions.contains(&ae2));
                    } else {
                        assert!(!folder(&next).join("mod_ae2.dll").exists());
                    }
                    if seeded.iter().any(|id| id == "hnn") {
                        assert_eq!(fs::read(folder(&next).join("mod_hnn.dll")).unwrap(), b"hnn");
                    }
                }
            });
        }
    });
    // Once the writers are done, every entry is whole. ftb's may be from another writer than
    // ae2's, built against another ae2: then it is a miss.
    let after = Kept::new(&root).seed(&dir.path().join("after"), &list, &keys);
    assert!(
        after == ["ae2", "ftb", "hnn"] || after == ["ae2", "hnn"],
        "{after:?}"
    );
}

#[test]
fn a_bad_blob_already_in_the_store_is_replaced_and_a_good_one_is_dated_now() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    let ae2 = libraries.iter().find(|l| l.name == "mod_ae2.dll").unwrap();
    let hnn = libraries.iter().find(|l| l.name == "mod_hnn.dll").unwrap();
    fs::create_dir_all(kept.blobs()).unwrap();
    // A blob named for ae2's bytes holding others, and an old but intact one for hnn.
    fs::write(kept.blobs().join(&ae2.sha256), b"not ae2").unwrap();
    fs::copy(&hnn.source, kept.blobs().join(&hnn.sha256)).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(kept.blobs().join(&hnn.sha256))
        .unwrap()
        .set_modified(old)
        .unwrap();
    kept.save(&work, &built, &libraries, &list, &keys).unwrap();
    assert_eq!(
        sha256_file(&kept.blobs().join(&ae2.sha256)).unwrap(),
        ae2.sha256
    );
    let dated = fs::metadata(kept.blobs().join(&hnn.sha256))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        dated > old,
        "an intact blob is dated now, so a prune keeps it"
    );
    assert_eq!(
        kept.seed(&dir.path().join("next"), &list, &keys),
        ["ae2", "ftb", "hnn"]
    );
}

#[test]
fn a_mod_without_its_fingerprint_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    fs::remove_file(folder(&work).join("mod_hnn.fingerprint")).unwrap();
    assert_eq!(
        kept.save(&work, &built, &libraries, &list, &keys).unwrap(),
        2
    );
    assert!(!kept.index_path(&keys["hnn"]).exists());
    assert!(kept.index_path(&keys["ae2"]).exists());
}

#[cfg(windows)]
#[test]
fn a_mod_without_an_import_library_is_still_kept() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::new(&dir.path().join("native"));
    let list = mods(&[]);
    let keys = keys(&build(), &list);
    let (work, built, libraries) = work_with(dir.path(), ALL);
    fs::remove_file(folder(&work).join("libmod_hnn.dll.a")).unwrap();
    assert_eq!(
        kept.save(&work, &built, &libraries, &list, &keys).unwrap(),
        3
    );
    let next = dir.path().join("next");
    assert_eq!(kept.seed(&next, &list, &keys), ["ae2", "ftb", "hnn"]);
    assert!(!folder(&next).join("libmod_hnn.dll.a").exists());
}

#[test]
fn a_mod_linking_to_one_that_comes_later_has_no_key() {
    let list = vec![
        Mod {
            id: "ftb",
            files: "ftb-files".into(),
            links: vec!["ae2"],
        },
        Mod {
            id: "ae2",
            files: "ae2-files".into(),
            links: vec![],
        },
    ];
    let keys = keys(&build(), &list);
    assert!(!keys.contains_key("ftb"));
    assert!(keys.contains_key("ae2"));
}
