use super::*;
#[test]
fn first_seen_stamp_ages_without_being_refreshed_by_inventory() {
    let home = crate::storage::tests::Home::new();
    home.seed("rust/old/payload");
    let root = Root::open(&home.0).unwrap();
    let entry = Entry::fixed(&root, Path::new("rust/old"), Category::RustCompiler).unwrap();
    let used = entry.last_used().unwrap().unwrap();
    assert_eq!(entry.last_used().unwrap(), Some(used));
    let units = [policy::Unit {
        bytes: 100,
        eligible: true,
        last_used: Some(used),
        tier: policy::Tier::Expiring,
    }];
    assert!(policy::select(&units, None, used).is_empty());
    assert_eq!(
        policy::select(
            &units,
            None,
            used + std::time::Duration::from_secs(7 * 86400)
        ),
        [0]
    );
}

#[test]
fn revision_names_are_parsed_not_searched() {
    let home = crate::storage::tests::Home::new();
    let root = Root::open(&home.0).unwrap();
    let sha = "0123456789abcdef0123456789abcdef01234567";
    for (path, expected) in [
        (format!("native/source/{sha}"), Some(sha)),
        (format!("native/source/not-{sha}"), None),
        (format!("native/tools/{}", &sha[..12]), Some(&sha[..12])),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v3-x86_64-unknown-linux-gnu"),
            Some(sha),
        ),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v3-x86_64-unknown-linux-gnu-server"),
            Some(sha),
        ),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v4-x86_64-unknown-linux-gnu"),
            Some(sha),
        ),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v4-x86_64-unknown-linux-gnu-server"),
            Some(sha),
        ),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v3-unknown-target"),
            None,
        ),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v4-unknown-target"),
            None,
        ),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v30-x86_64-unknown-linux-gnu"),
            None,
        ),
        (
            format!(
                "native/prebuilt/{sha}-sunrise-prebuilt-v4-x86_64-unknown-linux-gnu-server-server"
            ),
            None,
        ),
        (
            format!("native/prebuilt/{sha}-sunrise-prebuilt-v4-x86_64-unknown-linux-gnu.tar.gz"),
            None,
        ),
        (
            format!("native/prebuilt/not-{sha}-sunrise-prebuilt-v4-x86_64-unknown-linux-gnu"),
            None,
        ),
        // A build's SDK slices, and a developer install's laid-out game.
        (
            format!("native/prebuilt/{sha}-x86_64-unknown-linux-gnu"),
            Some(sha),
        ),
        (
            format!("native/prebuilt/{sha}-x86_64-unknown-linux-gnu-server"),
            Some(sha),
        ),
        (
            format!("native/prebuilt/{sha}-x86_64-unknown-linux-gnu.developer"),
            Some(sha),
        ),
        (format!("native/prebuilt/{sha}-unknown-target"), None),
        (
            format!("native/prebuilt/{sha}-x86_64-unknown-linux-gnu.partial"),
            None,
        ),
        (
            format!("native/internals-sync/catalog-{sha}-x86_64-unknown-linux-gnu"),
            Some(sha),
        ),
        (format!("native/internals-sync/garbage-{sha}"), None),
    ] {
        assert_eq!(
            Entry::fixed(&root, Path::new(&path), Category::BuildFiles)
                .unwrap()
                .revision(),
            expected
        );
    }
    // What an interrupted unpack or download left is known as such, and nothing else is.
    for (path, leftover) in [
        (format!("native/prebuilt/.{sha}-sunrise-prebuilt-v6-x86_64-unknown-linux-gnu.tar.gz.partial"), true),
        (format!("native/prebuilt/{sha}-x86_64-unknown-linux-gnu.lock"), true),
        ("native/prebuilt/runtime-x86_64-unknown-linux-gnu.tar.xz.download".to_owned(), true),
        (format!("native/prebuilt/{sha}-x86_64-unknown-linux-gnu"), false),
        (format!("native/source/{sha}.partial"), false),
    ] {
        let entry = Entry::fixed(&root, Path::new(&path), Category::BuildFiles).unwrap();
        assert_eq!(entry.leftover(), leftover, "{path}");
    }
}
#[test]
fn refused_units_are_residual_with_bytes_and_reason() {
    let home = crate::storage::tests::Home::new();
    let file = home.seed("game/output");
    let storage = Storage::at(&home.0).unwrap();
    let root = Root::open(&home.0).unwrap();
    let entries = vec![Entry::fixed(&root, Path::new("game"), Category::BuildFiles).unwrap()];
    let directory = file.parent().unwrap();
    // Give inline-directory filesystems enough names to allocate directory blocks too.
    for index in 0..32 {
        fs::write(
            directory.join(format!("padding-{index:02}-{}", "x".repeat(180))),
            [],
        )
        .unwrap();
    }
    // Independent filesystem allocations, not the recursive production sum.
    let own_bytes = |path: &Path| {
        super::super::allocation(path, &fs::symlink_metadata(path).unwrap())
            .unwrap()
            .1
    };
    let bytes = own_bytes(directory)
        + fs::read_dir(directory)
            .unwrap()
            .map(|entry| own_bytes(&entry.unwrap().path()))
            .sum::<u64>();
    let report = storage.apply_retaining(entries, None, true, true).unwrap();
    let row = report
        .rows
        .iter()
        .find(|row| row.category == Category::BuildFiles)
        .unwrap();
    assert_eq!(row.bytes, bytes);
    assert_eq!(row.freed_bytes, 0);
    assert_eq!(row.remaining_bytes, bytes);
    assert_eq!(row.kept.len(), 1);
    assert_eq!(row.kept[0].bytes, bytes);
    assert!(!row.kept[0].reason.is_empty());
    assert!(file.exists());
}

#[cfg(unix)]
#[test]
fn a_unit_with_an_unexpected_link_is_refused_on_its_row_and_holds_up_no_other() {
    let home = crate::storage::tests::Home::new();
    let good = home.seed("rust/old/payload");
    let bad = home.seed("cache/payload");
    home.seed("worlds/keep");
    std::os::unix::fs::symlink(home.0.join("worlds"), home.0.join("cache/link")).unwrap();
    let storage = Storage::at(&home.0).unwrap();
    let root = Root::open(&home.0).unwrap();
    let entries = vec![
        Entry::fixed(&root, Path::new("cache"), Category::OtherCaches).unwrap(),
        Entry::fixed(&root, Path::new("rust/old"), Category::RustCompiler).unwrap(),
    ];
    let report = storage.apply_retaining(entries, None, true, true).unwrap();
    let row = |category| {
        report
            .rows
            .iter()
            .find(|row| row.category == category)
            .unwrap()
    };
    let refused = row(Category::OtherCaches);
    assert_eq!((refused.bytes, refused.freed_bytes), (0, 0));
    assert_eq!(refused.kept.len(), 1);
    assert!(refused.kept[0].reason.contains("link"));
    assert!(bad.exists());
    assert!(home.0.join("worlds/keep").exists());
    assert!(row(Category::RustCompiler).freed_bytes > 0);
    assert!(row(Category::RustCompiler).kept.is_empty());
    assert!(!good.exists());
}

/// The walk the eviction pass made before one walk served every figure: a fresh recursive
/// walk with its own set of counted blocks, as `allocated` and `allocated_bytes` made it.
fn legacy_walk(path: &Path, seen: &mut HashSet<(u64, u64)>) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return 0,
    };
    let (id, bytes) = super::super::allocation(path, &meta).unwrap();
    if !seen.insert(id) {
        return 0;
    }
    let mut total = bytes;
    if meta.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            total += legacy_walk(&entry.unwrap().path(), seen);
        }
    }
    total
}

fn legacy_allocated(entries: &[Entry], category: Category) -> u64 {
    let mut seen = HashSet::new();
    entries
        .iter()
        .filter(|entry| entry.category == category)
        .map(|entry| legacy_walk(&entry.path, &mut seen))
        .sum()
}

#[test]
fn retained_compiler_bytes_use_budget_but_cannot_be_cleared() {
    let home = crate::storage::tests::Home::new();
    let cache = home.seed("cargo/registry/download");
    let storage = Storage::at(&home.0).unwrap();
    let now = std::time::SystemTime::now();
    let ordinary = storage.evict_inventory(now, None).unwrap();
    assert!(ordinary.total_bytes > 0 && cache.exists());
    let old_worker = home.seed("native/.nb-old/compiler/w");
    let older_launchers = home.seed("native/.native-attempt-older/compiler/work");
    let report = storage
        .evict_inventory(now, Some(ordinary.total_bytes))
        .unwrap();
    assert!(
        !cache.exists(),
        "retained worker must count toward the cache limit"
    );
    assert!(
        old_worker.is_file(),
        "the old worker is never a deletion candidate"
    );
    assert!(
        older_launchers.is_file(),
        "nor is one an earlier launcher left"
    );
    let protected = &report.rows[0];
    assert_eq!(protected.category, Category::BuildFiles);
    assert!(protected.remaining_bytes > 0);
    assert_eq!(protected.bytes, protected.remaining_bytes);
    assert_eq!(protected.clearable_bytes(), 0);
    assert_eq!(protected.kept[0].bytes, protected.remaining_bytes);
    let manual = storage.report(Some(Category::BuildFiles), false).unwrap();
    assert_eq!(manual.rows[0].clearable_bytes(), 0);
    assert!(old_worker.is_file());
}

#[test]
fn one_walk_per_folder_reports_what_the_repeated_walks_did() {
    let home = crate::storage::tests::Home::new();
    let shared = home.seed("rust/old/payload");
    home.seed("rust/old/deep/more");
    // A file shared by a cleared compiler folder and a kept build folder, and one shared by
    // two folders of the same row: each row counts a block once, whatever the other rows hold.
    fs::create_dir_all(home.0.join("native/source/not-a-revision")).unwrap();
    fs::hard_link(&shared, home.0.join("native/source/not-a-revision/shared")).unwrap();
    let twin = home.seed("rust/old2/own");
    fs::hard_link(&twin, home.0.join("rust/old2/twin")).unwrap();
    // Kept: recently used (cargo) and protected (a source folder with no revision in its name).
    home.seed("cargo/registry/crate");
    home.seed("native/source/not-a-revision/file");
    let storage = Storage::at(&home.0).unwrap();
    let before_entries = storage.entries().unwrap();
    let count = before_entries.len();
    let sizes: Vec<u64> = before_entries
        .iter()
        .map(|entry| legacy_walk(&entry.path, &mut HashSet::new()))
        .collect();
    let before: Vec<u64> = Category::ALL
        .iter()
        .map(|category| legacy_allocated(&before_entries, *category))
        .collect();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(8 * 86400);
    let walks_before = crate::storage::walks();
    let report = storage.evict_inventory(later, Some(25 << 30)).unwrap();
    let walked = crate::storage::walks() - walks_before;
    let entries = storage.entries().unwrap();
    let cleared = before_entries
        .iter()
        .zip(&sizes)
        .filter(|(entry, size)| {
            **size > 0 && legacy_walk(&entry.path, &mut HashSet::new()) != **size
        })
        .count();
    // Each folder once, and once more only the folders that were cleared.
    assert_eq!(walked, count + cleared);
    let mut total = 0;
    let mut kept_any = false;
    for (row, bytes) in report.rows.iter().zip(&before) {
        let remaining = legacy_allocated(&entries, row.category);
        assert_eq!(row.bytes, *bytes, "{:?}", row.category);
        assert_eq!(row.remaining_bytes, remaining, "{:?}", row.category);
        assert_eq!(row.freed_bytes, bytes.saturating_sub(remaining));
        // A folder left as it was (not cleared) and holding something is kept.
        let kept: Vec<u64> = before_entries
            .iter()
            .zip(&sizes)
            .filter(|(entry, _)| entry.category == row.category)
            .map(|(entry, size)| (legacy_walk(&entry.path, &mut HashSet::new()), *size))
            .filter(|(now, size)| now == size && *now > 0)
            .map(|(now, _)| now)
            .collect();
        assert_eq!(
            row.kept.iter().map(|kept| kept.bytes).collect::<Vec<_>>(),
            kept,
            "{:?}",
            row.category
        );
        kept_any |= !kept.is_empty();
        total += bytes;
    }
    assert_eq!(report.total_bytes, total);
    assert!(kept_any);
    assert!(home.0.join("cargo/registry/crate").exists());
}

#[test]
fn a_refused_unit_is_walked_once_and_a_cleared_one_twice() {
    let home = crate::storage::tests::Home::new();
    home.seed("rust/old/payload");
    let storage = Storage::at(&home.0).unwrap();
    let root = Root::open(&home.0).unwrap();
    #[cfg(unix)]
    {
        home.seed("cache/payload");
        std::os::unix::fs::symlink(home.0.join("rust"), home.0.join("cache/link")).unwrap();
    }
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut entries =
        vec![Entry::fixed(&root, Path::new("rust/old"), Category::RustCompiler).unwrap()];
    #[cfg(unix)]
    entries.push(Entry::fixed(&root, Path::new("cache"), Category::OtherCaches).unwrap());
    let count = entries.len();
    let walks = crate::storage::walks();
    let report = storage.apply_retaining(entries, None, true, true).unwrap();
    // The refused folder was walked once (and stopped there); the sound one once, then once
    // more after it was cleared.
    assert_eq!(crate::storage::walks() - walks, count + 1);
    assert!(report.rows[3].freed_bytes > 0);
    #[cfg(unix)]
    assert_eq!(report.rows[4].kept.len(), 1);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "manual allocated 40 GiB proof; requires SUNRISE_EVICTION_PROOF_ROOT in checkout scratch"]
fn allocated_forty_gib_cleanup_proof() {
    let path = PathBuf::from(
        std::env::var_os("SUNRISE_EVICTION_PROOF_ROOT").expect("isolated proof root"),
    );
    fs::create_dir_all(&path).unwrap();
    let home = crate::storage::tests::Home(path.canonicalize().unwrap());
    for (folder, gib) in [
        ("rust/old", 12),
        ("cargo", 12),
        ("tmp", 4),
        ("native/patched/proof", 4),
        ("versions/native-leased", 4),
        ("versions/native-legacy", 4),
    ] {
        let file = home.seed(&format!("{folder}/payload"));
        assert!(std::process::Command::new("fallocate")
            .args(["-l", &format!("{gib}G")])
            .arg(file)
            .status()
            .unwrap()
            .success());
    }
    assert_eq!(backend::data_dir().canonicalize().unwrap(), home.0);
    let leased = home.0.join("versions/native-leased");
    let legacy = home.0.join("versions/native-legacy");
    for directory in [&leased, &legacy] {
        fs::write(directory.join(".installed"), b"").unwrap();
    }
    fs::write(
        leased.join(backend::CLIENT),
        sunrise_launcher_smod::holder::CAPABILITY,
    )
    .unwrap();
    fs::write(legacy.join(backend::CLIENT), b"pre-holder game").unwrap();
    crate::run_leases::publish(&leased).unwrap();
    let registry = sunrise_launcher_smod::runtime_leases::Registry::existing(&home.0).unwrap();
    let _game = registry
        .lock(
            &sunrise_launcher_smod::runtime_leases::open_directory(&leased).unwrap(),
            sunrise_launcher_smod::locks::LockMode::Shared,
        )
        .unwrap();
    // The proof root is the launcher data folder itself (asserted above).
    let storage = Storage::at_in(&home.0, backend::data_dir(), backend::game_home()).unwrap();
    assert!(matches!(Cache::acquire(&home.0), Err(Error::Busy)));
    let before = allocated_bytes(&home.0).unwrap();
    let now = std::time::SystemTime::now();
    let first = storage.evict_inventory(now, Some(25 << 30)).unwrap();
    let later = now + std::time::Duration::from_secs(8 * 86400);
    let second = storage.evict_inventory(later, Some(25 << 30)).unwrap();
    let freed: u64 = first
        .rows
        .iter()
        .chain(&second.rows)
        .map(|row| row.freed_bytes)
        .sum();
    let residual = allocated_bytes(&home.0).unwrap();
    println!("allocated_before={before} inventoried_before={} freed={freed} allocated_residual={residual}", first.total_bytes);
    assert!(before >= 40 * (1 << 30));
    assert!(freed >= 28 * (1 << 30));
    assert!(residual >= 12 * (1 << 30));
    assert!(residual < 25 * (1 << 30));
    assert!(leased.join("payload").exists());
    assert!(legacy.join("payload").exists());
    assert!(home.0.join("native/patched/proof/payload").exists());
    assert!(second.rows.iter().any(|row| row
        .kept
        .iter()
        .any(|kept| kept.bytes >= 4 * (1 << 30) && !kept.reason.is_empty())));
    assert!(matches!(Cache::acquire(&home.0), Err(Error::Busy)));
}

#[test]
fn build_stamps_match_the_former_inventory_selection_without_walking_it() {
    let home = crate::storage::tests::Home::new();
    let version = crate::rust_toolchain::VERSION;
    let stamped = [
        "cargo".to_owned(),
        "tmp".to_owned(),
        format!("rust/{version}-x86_64-unknown-linux-gnu"),
        format!("rust/{version}-aarch64-apple-darwin"),
    ];
    for folder in &stamped {
        fs::create_dir_all(home.0.join(folder)).unwrap();
    }
    home.seed("rust/1.0.0-old/payload");
    home.seed(&format!("rust/{version}-not-a-dir"));
    home.seed("native/source/0123456789abcdef0123456789abcdef01234567/payload");
    home.seed(".test-game/instances/demo/.sunrise-cache/payload");
    let storage = Storage::at(&home.0).unwrap();

    let before = (walks(), entries_calls());
    storage.stamp_build().unwrap();
    assert_eq!((walks(), entries_calls()), before);

    fn collect(folder: &Path, stamps: &mut HashSet<PathBuf>) {
        for entry in fs::read_dir(folder).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                collect(&entry.path(), stamps);
            } else if entry.file_name() == ".last-used" {
                stamps.insert(entry.path());
            }
        }
    }
    let mut stamps = HashSet::new();
    collect(&home.0, &mut stamps);
    assert_eq!(
        stamps,
        stamped
            .map(|folder| home.0.join(folder).join(".last-used"))
            .into_iter()
            .collect()
    );
}

#[cfg(unix)]
#[test]
fn build_stamps_refuse_linked_cache_roots_and_toolchains() {
    use std::os::unix::fs::symlink;
    for relative in [
        "cargo",
        "tmp",
        "rust",
        &format!("rust/{}-link", crate::rust_toolchain::VERSION),
    ] {
        let home = crate::storage::tests::Home::new();
        let outside = crate::storage::tests::Home::new();
        let path = home.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&outside.0, &path).unwrap();
        let storage = Storage::at(&home.0).unwrap();
        assert!(matches!(storage.stamp_build(), Err(Error::Unsafe(link)) if link == path));
        assert!(!outside.0.join(".last-used").exists());
    }
}
