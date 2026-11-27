use super::*;
use crate::storage::{entries_calls, walks};

#[test]
fn build_completion_stamps_without_inventory_on_every_outcome() {
    const CHILD: &str = "SUNRISE_BUILD_COMPLETION_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_mods::tests::build_completion::build_completion_stamps_without_inventory_on_every_outcome",
            ])
            .env(CHILD, "1")
            .env("SUNRISE_LAUNCHER_HOME", home.path().join("launcher"))
            .env("SUNRISE_HOME", home.path().join("game"))
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }

    let home = backend::build_dir();
    let toolchain = format!("rust/{}-fixture", crate::rust_toolchain::VERSION);
    let profile = instances::create("Stamp fixture", None).unwrap();
    let profile_cache = profile.home.join(".sunrise-cache");
    let stamped = [home.join("cargo"), home.join("tmp"), home.join(toolchain)];
    let untouched = [
        home.join("rust/old"),
        home.join("native/source"),
        profile_cache,
    ];
    for path in stamped.iter().chain(&untouched) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("payload"), b"keep").unwrap();
    }
    let cancellation = Cancellation::default();
    let version = "native-stamp-fixture";
    for outcome in [Ok(Plan::Prebuilt), Ok(Plan::UpToDate), Err("failed")] {
        let profile = instances::find(&profile.id).unwrap();
        let before = (walks(), entries_calls());
        let actual = logged(&cancellation, &|| (), |cache| {
            cache.map_err(|error| Failed::Other(error.into()))?;
            let built = outcome
                .as_ref()
                .map(|plan| Built {
                    version: version.into(),
                    plan: plan.clone(),
                    profile: Profile::Optimized,
                    secs: 0,
                    left_out: Vec::new(),
                })
                .map_err(|error| Failed::Other((*error).into()));
            finish_profile_build(
                "fixture-base",
                &profile,
                &cancellation,
                built,
                &|_| {},
                || None,
            )
        });
        assert_eq!((walks(), entries_calls()), before);
        match outcome {
            Ok(_) => {
                assert!(matches!(actual, Ok(ClientBuild::Native(selected)) if selected == version))
            }
            Err(message) => {
                assert!(matches!(actual, Err(Failed::Other(error)) if error == message))
            }
        }
        assert_eq!(
            instances::find(&profile.id).unwrap().version.as_deref(),
            Some(version)
        );
        for path in &stamped {
            fs::remove_file(path.join(".last-used")).unwrap();
        }
        for path in &untouched {
            assert_eq!(fs::read(path.join("payload")).unwrap(), b"keep");
            assert!(!path.join(".last-used").exists());
        }
    }
}
