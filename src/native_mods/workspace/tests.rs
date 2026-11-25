use super::*;

use std::sync::atomic::Ordering;

const KEY: &str = "0123456789ab-01234567";

fn open(native: &Path, key: &str) -> (Workspace, Opened) {
    Workspace::open(native, key, &tool::Cancellation::default(), &|| {}).unwrap()
}

fn parts() -> KeyParts<'static> {
    KeyParts {
        revision: "0123456789abcdef0123",
        signed_sdk: "index",
        route: "prebuilt",
        target: "client",
        platform: "linux",
        rust_target: "x86_64-unknown-linux-gnu",
        mixins_from_source: false,
    }
}

#[test]
fn the_kept_folder_is_named_by_the_game_build_and_what_built_it() {
    let same = key(&parts());
    assert_eq!(same, key(&parts()));
    assert!(same.starts_with("0123456789ab-"));
    assert_eq!(same.len(), KEY_LENGTH);
    let others = [
        KeyParts {
            revision: "ffffffffffff",
            ..parts()
        },
        KeyParts {
            signed_sdk: "other",
            ..parts()
        },
        KeyParts {
            route: "prebuilt-api",
            ..parts()
        },
        KeyParts {
            target: "server",
            ..parts()
        },
        KeyParts {
            platform: "windows",
            ..parts()
        },
        KeyParts {
            rust_target: "aarch64-apple-darwin",
            ..parts()
        },
        KeyParts {
            mixins_from_source: true,
            ..parts()
        },
    ];
    for other in &others {
        assert_ne!(key(other), same);
    }
}

#[test]
fn a_second_open_reuses_the_folder_and_gives_a_fresh_tmp() {
    let native = tempfile::tempdir().unwrap();
    let (first, opened) = open(native.path(), KEY);
    assert_eq!(opened, Opened::Fresh);
    first
        .isolate(&mut std::process::Command::new("sdk-tool"))
        .unwrap();
    fs::write(first.path().join("target/x"), "built").unwrap();
    fs::write(first.path().join("cargo/y"), "fetched").unwrap();
    fs::write(first.path().join("tmp/z"), "scratch").unwrap();
    let kept = first.path().to_owned();
    drop(first);
    let (second, opened) = open(native.path(), KEY);
    assert_eq!(opened, Opened::Reused);
    assert_eq!(second.path(), kept);
    assert_eq!(fs::read(kept.join("target/x")).unwrap(), b"built");
    assert_eq!(fs::read(kept.join("cargo/y")).unwrap(), b"fetched");
    assert_eq!(fs::read_dir(kept.join("tmp")).unwrap().count(), 0);
}

#[test]
fn an_unproved_worker_sets_the_folder_aside_and_starts_clean() {
    let native = tempfile::tempdir().unwrap();
    let (mut first, _) = open(native.path(), KEY);
    fs::write(first.path().join("old"), "from the last build").unwrap();
    first.allow_worker().unwrap();
    let kept = first.path().to_owned();
    drop(first);
    let (second, opened) = open(native.path(), KEY);
    let Opened::SetAside(aside) = opened else {
        panic!("{opened:?}");
    };
    assert!(aside
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with(ATTEMPT_PREFIX));
    assert_eq!(fs::read(aside.join("old")).unwrap(), b"from the last build");
    assert_eq!(second.path(), kept);
    assert!(!kept.join("old").exists() && !kept.join(WORKER).exists());
    // This launcher can't prove the old tree gone, so Storage must not take it for a leftover.
    assert!(!attempt_owner::gone(&aside));
}

#[test]
fn a_proved_worker_leaves_the_folder_for_reuse() {
    let native = tempfile::tempdir().unwrap();
    let (mut first, _) = open(native.path(), KEY);
    first.allow_worker().unwrap();
    first.worker_finished().unwrap();
    drop(first);
    assert_eq!(open(native.path(), KEY).1, Opened::Reused);
}

#[test]
fn a_second_open_waits_for_the_lock_and_says_so() {
    let native = tempfile::tempdir().unwrap();
    let (held, _) = open(native.path(), KEY);
    let stopped = tool::Cancellation::default();
    let said = std::sync::atomic::AtomicUsize::new(0);
    let waiter = std::thread::scope(|scope| {
        let stop = stopped.clone();
        let canceller = scope.spawn(|| {
            while said.load(Ordering::SeqCst) == 0 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            stop.stop();
        });
        let result = Workspace::open(native.path(), KEY, &stopped, &|| {
            said.fetch_add(1, Ordering::SeqCst);
        });
        canceller.join().unwrap();
        result
    });
    assert_eq!(said.load(Ordering::SeqCst), 1);
    assert_eq!(waiter.err().unwrap(), super::super::CANCELLED);
    drop(held);
    assert_eq!(open(native.path(), KEY).1, Opened::Reused);
}

#[test]
fn the_mods_folder_path_is_stable_per_profile() {
    let native = tempfile::tempdir().unwrap();
    let (workspace, _) = open(native.path(), KEY);
    let one = workspace.mods(Path::new("/profiles/one"));
    assert_eq!(one, workspace.mods(Path::new("/profiles/one")));
    assert_ne!(one, workspace.mods(Path::new("/profiles/two")));
    assert!(one.starts_with(workspace.path()));
}

/// A cached SDK with an `api` slice of two files, one in a folder, and a `runtime` slice of one.
fn cached(dir: &Path) -> Package {
    let cached = Package::at(&dir.join("cache"));
    fs::create_dir_all(cached.api.join("deps")).unwrap();
    fs::create_dir_all(&cached.runtime).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    for file in [
        cached.api.join("top"),
        cached.api.join("deps/nested"),
        cached.runtime.join("game"),
    ] {
        fs::write(&file, b"slice").unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    cached
}

#[test]
fn the_package_is_linked_not_copied_and_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let cached = cached(dir.path());
    let laid = stage
        .lay_out_package(&cached, Some("sdk"), &|_| {})
        .unwrap();
    let file = laid.api.join("deps/nested");
    assert_eq!(fs::read(&file).unwrap(), b"slice");
    assert!(fs::metadata(&file).unwrap().permissions().readonly());
    assert!(!laid.engine.exists(), "a slice the SDK lacks stays absent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&file).unwrap().ino(),
            fs::metadata(cached.api.join("deps/nested")).unwrap().ino()
        );
    }
}

#[test]
fn laid_out_files_keep_the_source_modified_time() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let cached = cached(dir.path());
    // A read-only slice file keeps its time too.
    let top = cached.api.join("top");
    let mut readonly = fs::metadata(&top).unwrap().permissions();
    readonly.set_readonly(true);
    fs::set_permissions(&top, readonly).unwrap();
    let laid = stage
        .lay_out_package(&cached, Some("sdk"), &|_| {})
        .unwrap();
    // The tool's build folder name hashes these times: the same every build.
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    for file in [laid.api.join("top"), laid.api.join("deps/nested")] {
        assert_eq!(
            fs::metadata(&file).unwrap().modified().unwrap(),
            old,
            "{file:?}"
        );
    }
}

#[test]
fn a_second_build_lays_nothing_out_again() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let cached = cached(dir.path());
    let said = std::cell::Cell::new(0);
    let log = |_: &str| said.set(said.get() + 1);
    let laid = stage.lay_out_package(&cached, Some("sdk"), &log).unwrap();
    let stamp = stage.path().join("package.stamp");
    let stamped = fs::metadata(&stamp).unwrap().modified().unwrap();
    let again = stage.lay_out_package(&cached, Some("sdk"), &log).unwrap();
    assert_eq!(again, laid);
    assert_eq!(fs::metadata(&stamp).unwrap().modified().unwrap(), stamped);
    assert_eq!(said.get(), 0);
}

#[test]
fn a_changed_size_or_time_lays_the_package_out_again_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let cached = cached(dir.path());
    let heard = std::cell::RefCell::new(Vec::new());
    let log = |line: &str| heard.borrow_mut().push(line.to_owned());
    let laid = stage.lay_out_package(&cached, Some("sdk"), &log).unwrap();
    // Replaced by another file, not written through the link into the cache.
    let file = laid.api.join("top");
    fs::remove_file(&file).unwrap();
    fs::write(&file, b"tampered with, and longer").unwrap();
    stage.lay_out_package(&cached, Some("sdk"), &log).unwrap();
    assert_eq!(heard.borrow().len(), 1);
    assert!(heard.borrow()[0].contains("laying them out again"));
    assert_eq!(fs::read(&file).unwrap(), b"slice");
}

#[test]
fn a_developers_own_sdk_is_laid_out_each_time() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let cached = cached(dir.path());
    let laid = stage
        .lay_out_package(&cached, None, &|_| unreachable!())
        .unwrap();
    let marker = laid.api.join("marker");
    fs::write(&marker, b"left by the first build").unwrap();
    stage
        .lay_out_package(&cached, None, &|_| unreachable!())
        .unwrap();
    assert!(!marker.exists());
    assert!(!stage.path().join("package.stamp").exists());
}

#[test]
fn a_hard_linked_package_file_is_not_taken_for_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let cached = cached(dir.path());
    let laid = stage
        .lay_out_package(&cached, Some("sdk"), &|_| {})
        .unwrap();
    regular_under(stage.path(), &laid.runtime.join("game")).unwrap();
}

#[test]
fn a_kept_folder_with_read_only_files_can_be_removed() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let cached = cached(dir.path());
    stage
        .lay_out_package(&cached, Some("sdk"), &|_| {})
        .unwrap();
    fs::remove_dir_all(stage.path()).unwrap();
    assert!(!stage.path().exists());
    assert_eq!(fs::read(cached.api.join("top")).unwrap(), b"slice");
}

#[test]
fn registry_library_aliases_keep_their_exact_names_and_bytes_inside_private_roots() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let root = stage.path();
    let package = Package::at(&root.join("package"));
    let rust = crate::rust_toolchain::Toolchain {
        dir: dir.path().join("rust"),
        host: crate::rust_toolchain::hosts().first().unwrap(),
    };
    let work = stage.work();
    let target = work.join("prebuilt-build/registry-fixture/target");
    let release = target.join(rust.host.target).join("release");
    let binary = work
        .join("prebuilt-package/bin")
        .join(crate::backend::CLIENT);
    let patch = release.join("libsunrise_patch.so");
    let mod_output = release.join("libgreeter.so");
    let output = target.join("deps/libfrom_target.so.2");
    let dependency = root.join("cargo/registry/src/thing/libfrom_cargo.so.1");
    let outside = root.join("tmp/libfrom_cargo.so.1");
    for file in [&binary, &patch, &mod_output, &output, &dependency, &outside] {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"compiled bytes").unwrap();
    }
    let rust_version = crate::rust_toolchain::VERSION;
    let make_record = |file: &Path, kind, id: &str, version: &str| tool::LibraryRecord {
        file: file.file_name().unwrap().to_str().unwrap().into(),
        sha256: sha256_file(file).unwrap(),
        kind,
        mod_id: id.into(),
        identity: format!("fixture-build {id} {version} {rust_version} unwind"),
    };
    let mut built = tool::Built {
        route: tool::CheckRoute::Prebuilt,
        binary,
        libraries: vec![
            mod_output.clone(),
            patch.clone(),
            output,
            dependency.clone(),
        ],
        records: vec![
            make_record(&mod_output, tool::LibraryKind::Mod, "greeter", "1.0.0"),
            make_record(&patch, tool::LibraryKind::Patch, "sunrise_patch", "0"),
        ],
        mods: vec!["greeter".into()],
        internals: Default::default(),
    };
    let manifest = Manifest::parse("id = 'greeter'\nname = 'Greeter'\nversion = '1.0.0'\napi = '0.2'\ncapabilities = ['registries']\n[native]\ngame = '26.3'\nsunrise = 'a123456789012345678901234567890123456789'\n").unwrap();
    let verify = |built: &tool::Built| {
        stage.verify_outputs(
            built,
            &package,
            &rust,
            Some(tool::CheckRoute::Prebuilt),
            "fixture-build",
            [&manifest].into_iter(),
        )
    };
    #[cfg(unix)]
    {
        let alias = dependency.with_extension("");
        std::os::unix::fs::symlink(dependency.file_name().unwrap(), &alias).unwrap();
        built.libraries.push(alias);
    }
    let accepted = verify(&built).unwrap();
    let installed = dir.path().join("private-install");
    fs::create_dir(&installed).unwrap();
    for library in &accepted {
        library.copy_into(&installed).unwrap();
    }
    assert_eq!(
        fs::read(installed.join("libfrom_cargo.so.1")).unwrap(),
        b"compiled bytes"
    );
    assert_eq!(
        fs::read(installed.join("libfrom_target.so.2")).unwrap(),
        b"compiled bytes"
    );
    #[cfg(unix)]
    assert!(!fs::symlink_metadata(installed.join("libfrom_cargo.so"))
        .unwrap()
        .file_type()
        .is_symlink());
    fs::write(&dependency, b"substituted after verification").unwrap();
    let library = accepted
        .iter()
        .find(|row| row.name == "libfrom_cargo.so.1")
        .unwrap();
    assert!(library.copy_into(&installed).is_err());
    built.libraries[3] = outside.clone();
    assert!(
        verify(&built).is_err(),
        "neither the rest of the private stage nor outside is a Cargo root"
    );
    #[cfg(unix)]
    {
        let escaping = dependency.with_file_name("escaping.so.1");
        std::os::unix::fs::symlink(&outside, &escaping).unwrap();
        built.libraries[3] = escaping;
        assert!(
            verify(&built).is_err(),
            "a Cargo-home alias cannot escape its private roots"
        );
    }
}

#[test]
fn an_sdk_can_only_supply_its_own_files_and_the_exact_requested_mods() {
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let package = Package::at(&stage.path().join("package"));
    let rust = crate::rust_toolchain::Toolchain {
        dir: dir.path().join("rust"),
        host: crate::rust_toolchain::hosts().first().unwrap(),
    };
    let work = stage.work();
    let bin = package.runtime.join("bin");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(crate::backend::CLIENT);
    let lib = work.join("greeter.dll");
    let outside = dir.path().join("outside.dll");
    for file in [&binary, &lib, &outside] {
        fs::write(file, b"bytes").unwrap();
    }
    let mut built = tool::Built {
        route: tool::CheckRoute::PrebuiltApi,
        binary: binary.clone(),
        libraries: vec![lib.clone()],
        mods: vec!["greeter".into()],
        records: vec![tool::LibraryRecord {
            file: "greeter.dll".into(),
            sha256: sha256_file(&lib).unwrap(),
            kind: tool::LibraryKind::Mod,
            mod_id: "greeter".into(),
            identity: format!(
                "fixture-build greeter 1.0.0 {} unwind",
                crate::rust_toolchain::VERSION
            ),
        }],
        internals: Default::default(),
    };
    let manifest = Manifest::parse(
        "id = 'greeter'\nname = 'Greeter'\nversion = '1.0.0'\napi = '0.2'\ncapabilities = ['registries']\n[native]\ngame = '26.3'\nsunrise = 'a123456789012345678901234567890123456789'\n",
    )
    .unwrap();
    let verify = |built: &tool::Built| {
        stage.verify_outputs(
            built,
            &package,
            &rust,
            Some(tool::CheckRoute::PrebuiltApi),
            "fixture-build",
            [&manifest].into_iter(),
        )
    };
    assert!(verify(&built).is_ok());
    built.route = tool::CheckRoute::Prebuilt;
    assert!(
        verify(&built).is_err(),
        "checked API route cannot publish an engine result"
    );
    built.route = tool::CheckRoute::PrebuiltApi;
    let fake_binary = work.join("another-executable");
    fs::write(&fake_binary, b"wrong executable").unwrap();
    built.binary = fake_binary;
    assert!(
        verify(&built).is_err(),
        "API result must use the SDK runtime executable"
    );
    built.binary = binary;
    fs::write(&lib, b"different output").unwrap();
    assert!(
        verify(&built).is_err(),
        "library record digest binds the copied output"
    );
    fs::write(&lib, b"bytes").unwrap();
    built.records[0].file = "another.dll".into();
    assert!(
        verify(&built).is_err(),
        "a library record names exactly one supplied path"
    );
    built.records[0].file = "greeter.dll".into();
    let identity = std::mem::replace(&mut built.records[0].identity, "wrong identity".into());
    assert!(
        verify(&built).is_err(),
        "records must name the exact build and mod version"
    );
    built.records[0].identity = identity;
    assert!(verify(&built).is_ok());
    let records = std::mem::take(&mut built.records);
    assert!(
        verify(&built).is_err(),
        "every requested mod needs its own record"
    );
    built.records = records;
    built.mods.push("unexpected".into());
    assert!(verify(&built).is_err());
    built.mods.pop();
    built.libraries.push(outside.clone());
    assert!(verify(&built).is_err());
    built.libraries[1] = lib;
    assert!(verify(&built).is_err());
    #[cfg(unix)]
    {
        let linked = work.join("linked.dll");
        std::os::unix::fs::symlink(outside, &linked).unwrap();
        built.libraries = vec![linked];
        assert!(verify(&built).is_err());
    }
}

/// Since sunrise#5136 a library's identity also names its mod's files and, for each native mod it
/// links, the SHA-256 of the identity that mod's library was built with.
#[test]
fn a_library_names_the_identities_of_the_mods_it_links() {
    use sunrise_launcher_smod::index::library_identity::library_identity;
    let dir = tempfile::tempdir().unwrap();
    let stage = Workspace::scratch(dir.path(), "scratch");
    let package = Package::at(&stage.path().join("package"));
    let rust = crate::rust_toolchain::Toolchain {
        dir: dir.path().join("rust"),
        host: crate::rust_toolchain::hosts().first().unwrap(),
    };
    let work = stage.work();
    let bin = package.runtime.join("bin");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(crate::backend::CLIENT);
    fs::write(&binary, b"bytes").unwrap();
    let rust_version = crate::rust_toolchain::VERSION;
    // As the tool stamps them: `base` first, then `dependent`, which links it.
    let base = format!(
        "{} unwind",
        library_identity(
            ["fixture-build", "base", "1.0.0", rust_version],
            "s1+g1",
            []
        )
    );
    let dependent = |base: &str| {
        format!(
            "{} unwind",
            library_identity(
                ["fixture-build", "dependent", "2.0.0", rust_version],
                "s2",
                [("base", base)]
            )
        )
    };
    let mut built = tool::Built {
        route: tool::CheckRoute::PrebuiltApi,
        binary,
        libraries: Vec::new(),
        mods: vec!["base".into(), "dependent".into()],
        records: Vec::new(),
        internals: Default::default(),
    };
    for (id, identity) in [("base", base.clone()), ("dependent", dependent(&base))] {
        let lib = work.join(format!("{id}.dll"));
        fs::write(&lib, id.as_bytes()).unwrap();
        built.records.push(tool::LibraryRecord {
            file: format!("{id}.dll"),
            sha256: sha256_file(&lib).unwrap(),
            kind: tool::LibraryKind::Mod,
            mod_id: id.into(),
            identity,
        });
        built.libraries.push(lib);
    }
    let manifest = |id: &str, version: &str| {
        Manifest::parse(&format!(
            "id = '{id}'\nname = 'M'\nversion = '{version}'\n[native]\ngame = '26.3'\nsunrise = 'a123456789012345678901234567890123456789'\n"
        ))
        .unwrap()
    };
    let manifests = [manifest("base", "1.0.0"), manifest("dependent", "2.0.0")];
    let verify = |built: &tool::Built| {
        stage.verify_outputs(
            built,
            &package,
            &rust,
            Some(tool::CheckRoute::PrebuiltApi),
            "fixture-build",
            manifests.iter(),
        )
    };
    assert!(verify(&built).is_ok());
    let mut refused = |identity: String, why: &str| {
        let kept = std::mem::replace(&mut built.records[1].identity, identity);
        assert!(verify(&built).is_err(), "{why}");
        built.records[1].identity = kept;
    };
    refused(
        dependent(&base.replace("s1+g1", "s1b+g1")),
        "built against another build of base",
    );
    refused(
        dependent(&base).replace("base=", "absent="),
        "links a mod this build did not make",
    );
    refused(
        dependent(&base).replace(" base=", " base"),
        "a malformed dependency list",
    );
    refused(
        dependent(&base).replace(" unwind", " abort"),
        "another panic strategy",
    );
    refused(
        dependent(&base).replace("fixture-build", "other-build"),
        "another game build",
    );
    refused(
        dependent(&base).replace(" 2.0.0 ", " 2.0.1 "),
        "another mod version",
    );
    refused(
        format!(
            "{} unwind extra",
            dependent(&base).trim_end_matches(" unwind")
        ),
        "an eighth field",
    );
    // An SDK from before sunrise#5136 stamps the five-field identity.
    for (row, (id, version)) in built
        .records
        .iter_mut()
        .zip([("base", "1.0.0"), ("dependent", "2.0.0")])
    {
        row.identity = format!("fixture-build {id} {version} {rust_version} unwind");
    }
    assert!(verify(&built).is_ok());
}

#[test]
fn a_build_too_deep_for_windows_is_found_before_it_starts() {
    let home = |user: &str| {
        std::path::PathBuf::from(format!(
            r"C:\Users\{user}\AppData\Roaming\sunrise\launcher\native"
        ))
    };
    // #396's player, eight letters, still has room beside the kept folder's longer path.
    assert_eq!(too_deep(&home("abcdefgh")), None);
    assert_eq!(too_deep(&home(&"u".repeat(9))), None);
    assert_eq!(too_deep(&home(&"u".repeat(10))), Some(260));
}
