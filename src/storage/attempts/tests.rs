use super::*;
use crate::native_mods::attempt_owner::FILE;
use crate::storage::tests::Home;

/// What a running build holds on its attempt folder: an open `.owner` file with its lock taken.
fn claimed(folder: &Path) -> fs::File {
    let file = fs::File::create(folder.join(FILE)).unwrap();
    file.try_lock().unwrap();
    file
}

/// What the Linux process scan says about `folder`, for a failed assertion's message.
fn scan(folder: &Path) -> String {
    #[cfg(target_os = "linux")]
    return format!("process scan: {:?}", super::process_uses(folder));
    #[cfg(not(target_os = "linux"))]
    return format!("no process scan for {}", folder.display());
}

fn build_files(report: &Report) -> &Row {
    report
        .rows
        .iter()
        .find(|row| row.category == Category::BuildFiles)
        .unwrap()
}

#[test]
fn leftover_attempt_without_owner_file_is_removed_by_cleanup() {
    let home = Home::new();
    let old = home.seed("native/.nb-old/package/x");
    let older = home.seed("native/.native-attempt-a/w/y");
    let storage = Storage::at(&home.0).unwrap();
    let report = storage
        .evict_inventory(std::time::SystemTime::now(), None)
        .unwrap();
    assert!(
        !old.exists() && !older.exists(),
        "{}; {}",
        scan(&home.0.join("native/.nb-old")),
        scan(&home.0.join("native/.native-attempt-a"))
    );
    let row = build_files(&report);
    assert!(row.freed_bytes > 0);
    assert!(row.kept.is_empty());
    assert!(crate::storage::eviction::freed_message(&report).is_some());
}

#[test]
fn attempt_with_a_live_owner_lock_is_kept() {
    let home = Home::new();
    let file = home.seed("native/.nb-live/f");
    let storage = Storage::at(&home.0).unwrap();
    let owner = claimed(file.parent().unwrap());
    let now = std::time::SystemTime::now();
    let report = storage.evict_inventory(now, None).unwrap();
    assert!(file.exists());
    let row = build_files(&report);
    assert!(row.remaining_bytes > 0);
    assert_eq!(row.clearable_bytes(), 0);
    assert_eq!(
        row.kept[0].reason,
        crate::i18n::tr("Old build files from an earlier launcher are kept until the launcher can prove no build still uses them. They are removed on the next cleanup once it can.")
    );
    drop(owner);
    storage.evict_inventory(now, None).unwrap();
    assert!(!file.exists(), "{}", scan(file.parent().unwrap()));
}

#[test]
fn explicit_clear_removes_leftover_attempts_but_a_plain_report_does_not() {
    let home = Home::new();
    let file = home.seed("native/.nb-old/package/x");
    let storage = Storage::at(&home.0).unwrap();
    storage.report(None, false).unwrap();
    assert!(file.exists());
    storage.report(Some(Category::BuildFiles), false).unwrap();
    assert!(!file.exists(), "{}", scan(&home.0.join("native/.nb-old")));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "daily"]
fn linux_process_using_attempt_blocks_removal() {
    let home = Home::new();
    let file = home.seed("native/.nb-busy/f");
    let mut worker = std::process::Command::new("sleep")
        .arg("30")
        .current_dir(file.parent().unwrap())
        .spawn()
        .unwrap();
    let storage = Storage::at(&home.0).unwrap();
    let now = std::time::SystemTime::now();
    storage.evict_inventory(now, None).unwrap();
    assert!(file.exists());
    worker.kill().unwrap();
    worker.wait().unwrap();
    storage.evict_inventory(now, None).unwrap();
    assert!(!file.exists());
}
