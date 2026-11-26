use super::*;
use crate::native_mods::tests::{dir, scratch_state, write_bundle};

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "native-aaaaaaa-base00000000";

/// An instance with two native mods, built on a native build whose record names Sunrise `A`.
struct Set {
    folder: PathBuf,
    built_on: About,
}

impl Set {
    fn new(name: &str) -> Self {
        let folder = dir(name).join("instance").join("native");
        write_bundle(&folder.join("alpha"), "alpha", "pub fn alpha() {}\n");
        write_bundle(&folder.join("beta"), "beta", "pub fn beta() {}\n");
        Self {
            folder,
            built_on: About {
                base: "26.3-0.1.0".into(),
                sunrise: A.into(),
                target: Target::Client,
                left_out: Vec::new(),
            },
        }
    }

    fn lookup(
        &self,
        platform: BuildPlatform,
        target: Target,
        options: &BuildOptions<'_>,
    ) -> Option<Hit> {
        lookup(&Request {
            base: BASE,
            folder: &self.folder,
            built_on: Some(&self.built_on),
            platform,
            profile: Profile::Optimized,
            target,
            options,
        })
    }

    fn found(&self) -> Option<String> {
        self.lookup(
            BuildPlatform::default(),
            Target::Client,
            &BuildOptions::default(),
        )
        .map(|hit| hit.version)
    }

    /// The key `pipeline` keeps for this set once its planned build is installed: from the
    /// staged copies of the mods, as it reads them before `materialize`.
    fn planned_key(&self) -> String {
        let copies = dir("reuse-copies");
        let requested: Vec<Bundle> = scan_bundles(&self.folder)
            .unwrap()
            .into_iter()
            .map(|bundle| {
                let to = copies.join(&bundle.manifest.id);
                copy_bundle(&bundle, &to).unwrap();
                Bundle { dir: to, ..bundle }
            })
            .collect();
        let files = files(&requested).unwrap();
        let (left_out, mods, _) = kept_mods(&requested, &[], side_of(Target::Client));
        Inputs {
            revision: &self.built_on.sunrise,
            base: BASE,
            origin: &self.built_on.base,
            profile: Profile::Optimized,
            target: Target::Client,
            platform: BuildPlatform::default(),
            exact: false,
            left_out: &left_out,
            mods: &mods,
            files: &files,
        }
        .key()
    }
}

/// An installed client build `version` with a record of what it was built from.
fn install_fake(version: &str) -> PathBuf {
    let dir = backend::version_dir(version);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(RECORD),
        format!(r#"{{"base":"26.3-0.1.0","sunrise":"{A}","target":"client","left_out":[]}}"#),
    )
    .unwrap();
    fs::write(dir.join(".installed"), "").unwrap();
    dir
}

/// A set planned and installed once is found again from its inputs alone, and used without the
/// SDK index, the check or any other step but saying it is up to date.
#[test]
fn the_same_inputs_find_the_installed_build_without_planning_it() {
    let _guard = scratch_state();
    let set = Set::new("reuse-hit");
    let version = format!("native-{}-000000000001", &A[..7]);
    let installed_dir = install_fake(&version);
    assert!(set.found().is_none(), "no record, no fast path");
    record(Target::Client, &version, &set.planned_key());
    let hit = set
        .lookup(
            BuildPlatform::default(),
            Target::Client,
            &BuildOptions::default(),
        )
        .expect("the same inputs find the build");
    assert_eq!(hit.version, version);
    let index_kept = sdk_cache::build_folder(A).exists();
    let steps = std::cell::RefCell::new(Vec::new());
    let built = reused(
        hit,
        Profile::Optimized,
        Target::Client,
        &BuildOptions::default(),
        &|step| steps.borrow_mut().push(step),
        std::time::Instant::now(),
    )
    .unwrap();
    assert_eq!(
        (built.version.as_str(), built.plan),
        (version.as_str(), Plan::UpToDate)
    );
    assert!(built.left_out.is_empty());
    assert_eq!(
        steps.into_inner(),
        [Step::Plan(Plan::UpToDate, Profile::Optimized)],
        "nothing is checked or fetched"
    );
    assert_eq!(
        sdk_cache::build_folder(A).exists(),
        index_kept,
        "no SDK index was fetched"
    );
    assert!(installed_dir.join(".installed").is_file());
    fs::remove_dir_all(installed_dir).unwrap();
}

/// Any input that changes misses the fast path, and the same input again finds it again.
#[test]
fn a_changed_mod_pin_base_key_index_or_set_misses_the_installed_build() {
    let _guard = scratch_state();
    let set = Set::new("reuse-miss");
    let version = format!("native-{}-000000000002", &A[..7]);
    let installed_dir = install_fake(&version);
    record(Target::Client, &version, &set.planned_key());
    assert_eq!(set.found().as_deref(), Some(version.as_str()));

    let source = set.folder.join("alpha").join("src/lib.rs");
    fs::write(&source, "pub fn alpha() { changed() }\n").unwrap();
    assert!(set.found().is_none(), "a changed mod file is another build");
    fs::write(&source, "pub fn alpha() {}\n").unwrap();
    assert_eq!(set.found().as_deref(), Some(version.as_str()));

    let manifest = set.folder.join("beta").join(NATIVE_MANIFEST);
    let pinned = fs::read_to_string(&manifest).unwrap();
    let repinned = pinned.replace(
        &format!("mixins='{}'", "b".repeat(40)),
        &format!("mixins='{}'", "c".repeat(40)),
    );
    assert_ne!(repinned, pinned, "the fixture pins horizon-mixins");
    fs::write(&manifest, repinned).unwrap();
    assert!(
        set.found().is_none(),
        "another horizon-mixins pin is another build"
    );
    fs::write(&manifest, &pinned).unwrap();
    assert_eq!(set.found().as_deref(), Some(version.as_str()));

    let windows = BuildPlatform {
        target: Some(rust_toolchain::WINDOWS_X64_TARGET),
        machine: None,
    };
    let options = BuildOptions::default();
    assert!(
        set.lookup(windows, Target::Client, &options).is_none(),
        "another base key"
    );
    assert!(
        set.lookup(BuildPlatform::default(), Target::Server, &options)
            .is_none(),
        "a server"
    );
    let other_base = Set {
        folder: set.folder.clone(),
        built_on: About {
            base: "26.3-0.1.1".into(),
            ..set.built_on.clone()
        },
    };
    assert!(
        other_base.found().is_none(),
        "another release to start from"
    );

    let bundle = scan_bundles(&set.folder).unwrap().remove(0);
    let skip = [LeftOut::new(&bundle, isolate::Why::DidNotBuild)];
    let skipping = BuildOptions {
        skip: &skip,
        ..BuildOptions::default()
    };
    assert!(
        set.lookup(BuildPlatform::default(), Target::Client, &skipping)
            .is_none(),
        "a build without one of the mods"
    );

    let index = sdk_cache::build_folder(A).join(sunrise_launcher_sdk::INDEX);
    fs::create_dir_all(index.parent().unwrap()).unwrap();
    fs::write(&index, b"{}").unwrap();
    assert!(set.found().is_none(), "another SDK index for the pin");
    fs::remove_dir_all(sdk_cache::build_folder(A)).unwrap();
    assert_eq!(set.found().as_deref(), Some(version.as_str()));

    fs::remove_file(installed_dir.join(".installed")).unwrap();
    assert!(set.found().is_none(), "a build that isn't installed");
    fs::remove_dir_all(installed_dir).unwrap();
}

/// The record keeps the newest keys, each once.
#[test]
fn a_record_keeps_the_newest_keys_once() {
    let _guard = scratch_state();
    let version = format!("native-{}-000000000003", &A[..7]);
    let installed_dir = install_fake(&version);
    for n in 0..10 {
        record(Target::Client, &version, &format!("key{n}"));
    }
    record(Target::Client, &version, "key5");
    let text = fs::read_to_string(installed_dir.join(INPUTS)).unwrap();
    let keys: Vec<&str> = text.lines().collect();
    assert_eq!(
        keys,
        ["key2", "key3", "key4", "key6", "key7", "key8", "key9", "key5"]
    );
    assert_eq!(
        installed(Target::Client, A, "key5").as_deref(),
        Some(version.as_str())
    );
    assert!(
        installed(Target::Client, A, "key1").is_none(),
        "an old key is gone"
    );
    fs::remove_dir_all(installed_dir).unwrap();
}
