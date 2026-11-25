use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    shared: std::path::PathBuf,
    compiled: std::path::PathBuf,
    workspace: Workspace,
}

/// A kept folder, a shared cargo home with one crate and one index file, and one mod's folder.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared");
    fs::create_dir_all(shared.join("registry/cache/idx")).unwrap();
    fs::create_dir_all(shared.join("registry/index/idx/.cache/a")).unwrap();
    fs::create_dir_all(shared.join("registry/src/idx/a-1.0")).unwrap();
    fs::write(shared.join("registry/cache/idx/a-1.0.crate"), b"crate a").unwrap();
    fs::write(shared.join("registry/index/idx/.cache/a/a"), b"index a").unwrap();
    fs::write(shared.join("registry/src/idx/a-1.0/lib.rs"), b"source").unwrap();
    let compiled = dir.path().join("compiled");
    fs::create_dir_all(compiled.join("greeter")).unwrap();
    fs::create_dir_all(dir.path().join("native")).unwrap();
    let workspace = Workspace::scratch(&dir.path().join("native"), "0123456789ab-01234567");
    Fixture {
        _dir: dir,
        shared,
        compiled,
        workspace,
    }
}

fn lock(fixture: &Fixture, text: &str) {
    fs::write(fixture.compiled.join("greeter/Cargo.lock"), text).unwrap();
}

#[test]
fn the_crates_are_linked_and_the_index_copied_but_no_sources_come_along() {
    let mut f = fixture();
    lock(&f, "");
    let filled = f
        .workspace
        .fill_cargo(&f.shared, &f.compiled, None)
        .unwrap();
    assert_eq!((filled.linked, filled.copied), (1, 1));
    let own = f.workspace.path().join("cargo/registry");
    let (shared_crate, own_crate) = (
        f.shared.join("registry/cache/idx/a-1.0.crate"),
        own.join("cache/idx/a-1.0.crate"),
    );
    assert_eq!(fs::read(&own_crate).unwrap(), b"crate a");
    assert_eq!(
        fs::read(own.join("index/idx/.cache/a/a")).unwrap(),
        b"index a"
    );
    assert!(!own.join("src").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ino = |file: &Path| fs::metadata(file).unwrap().ino();
        assert_eq!(ino(&own_crate), ino(&shared_crate));
        assert_ne!(
            ino(&own.join("index/idx/.cache/a/a")),
            ino(&f.shared.join("registry/index/idx/.cache/a/a"))
        );
    }
}

#[test]
fn a_file_already_in_the_folder_is_left_and_a_link_in_the_shared_cache_is_refused() {
    let mut f = fixture();
    lock(&f, "");
    let own = f.workspace.path().join("cargo/registry/cache/idx");
    fs::create_dir_all(&own).unwrap();
    fs::write(own.join("a-1.0.crate"), b"already here").unwrap();
    let filled = f
        .workspace
        .fill_cargo(&f.shared, &f.compiled, None)
        .unwrap();
    assert_eq!(filled.linked, 0);
    assert_eq!(fs::read(own.join("a-1.0.crate")).unwrap(), b"already here");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            f.shared.join("registry/cache/idx/a-1.0.crate"),
            f.shared.join("registry/cache/idx/b-1.0.crate"),
        )
        .unwrap();
        assert!(f
            .workspace
            .fill_cargo(&f.shared, &f.compiled, None)
            .is_err());
    }
}

#[test]
fn only_a_crate_the_lock_pins_is_shared() {
    let mut f = fixture();
    lock(&f, &format!("checksum = \"{}\"\n", sha256(b"pinned")));
    f.workspace
        .fill_cargo(&f.shared, &f.compiled, None)
        .unwrap();
    let downloaded = f.workspace.path().join("cargo/registry/cache/idx");
    fs::write(downloaded.join("pinned-1.0.crate"), b"pinned").unwrap();
    fs::write(downloaded.join("planted-1.0.crate"), b"planted").unwrap();
    let counts = f.workspace.publish_downloads(&f.shared).unwrap();
    assert_eq!(counts, (1, 1));
    let shared = f.shared.join("registry/cache/idx");
    assert_eq!(
        fs::read(shared.join("pinned-1.0.crate")).unwrap(),
        b"pinned"
    );
    assert!(!shared.join("planted-1.0.crate").exists());
}

#[test]
fn the_fetch_goes_offline_only_for_the_same_lock_and_cache_after_a_good_build() {
    let mut f = fixture();
    let offline = |f: &Fixture| {
        let mut command = std::process::Command::new("sdk-tool");
        f.workspace.isolate(&mut command).unwrap();
        command
            .get_envs()
            .any(|(name, value)| name == "CARGO_NET_OFFLINE" && value.is_some())
    };
    lock(&f, "one");
    assert!(
        !f.workspace
            .fill_cargo(&f.shared, &f.compiled, Some("sdk"))
            .unwrap()
            .offline
    );
    assert!(!offline(&f));
    // A build that failed leaves no stamp behind.
    assert!(
        !f.workspace
            .fill_cargo(&f.shared, &f.compiled, Some("sdk"))
            .unwrap()
            .offline
    );
    f.workspace.publish_downloads(&f.shared).unwrap();
    assert!(
        f.workspace
            .fill_cargo(&f.shared, &f.compiled, Some("sdk"))
            .unwrap()
            .offline
    );
    assert!(offline(&f));
    f.workspace.publish_downloads(&f.shared).unwrap();
    // Another lock, another game build, or a file new in the shared cache: fetch again.
    lock(&f, "two");
    assert!(
        !f.workspace
            .fill_cargo(&f.shared, &f.compiled, Some("sdk"))
            .unwrap()
            .offline
    );
    f.workspace.publish_downloads(&f.shared).unwrap();
    assert!(
        !f.workspace
            .fill_cargo(&f.shared, &f.compiled, Some("other"))
            .unwrap()
            .offline
    );
    f.workspace.publish_downloads(&f.shared).unwrap();
    fs::write(f.shared.join("registry/cache/idx/c-1.0.crate"), b"new").unwrap();
    assert!(
        !f.workspace
            .fill_cargo(&f.shared, &f.compiled, Some("other"))
            .unwrap()
            .offline
    );
}
