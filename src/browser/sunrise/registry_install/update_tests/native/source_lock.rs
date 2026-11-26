use super::*;

/// `native-build <FOLDER>`: a mod's source with C sources and a build script, but no registry
/// lock yet, builds with the lock `native-lock` writes, made in the build's copy only.
#[test]
#[ignore = "daily; build the launcher binary first"]
fn a_source_folder_without_a_lock_builds_with_the_one_native_lock_writes() {
    Fixture::run(
        concat!(
            module_path!(),
            "::a_source_folder_without_a_lock_builds_with_the_one_native_lock_writes"
        ),
        |fixture| {
            configure_tool(&fixture);
            let source = fixture.root.join("source").join(ID);
            fs::create_dir_all(source.join("src")).unwrap();
            fs::create_dir_all(source.join("csrc")).unwrap();
            fs::write(
                source.join("sunrise-mod.toml"),
                format!(
                    "id='{ID}'\nname='Native Lantern'\nversion='1.0.0'\napi='{}'\nside='both'\n[native]\nsunrise='{}'\nmixins='{}'\ngame='{}'\n",
                    sunrise_launcher_smod::manifest::API,
                    "a".repeat(40),
                    "c".repeat(40),
                    backend::PINNED_GAME
                ),
            )
            .unwrap();
            fs::write(
                source.join("Cargo.toml"),
                format!("[package]\nname='{ID}'\nversion='1.0.0'\nbuild='build.rs'\n"),
            )
            .unwrap();
            fs::write(source.join("build.rs"), "fn main() {}\n").unwrap();
            fs::write(source.join("csrc/shim.c"), "int shim(void) { return 1; }\n").unwrap();
            fs::write(source.join("src/lib.rs"), SOURCE).unwrap();
            let built = native_mods::build(
                BUILD,
                &source,
                &fixture.instance,
                None,
                false,
                &native_mods::Cancellation::default(),
                |_| {},
                || None,
            )
            .unwrap();
            assert!(
                matches!(built, native_mods::ClientBuild::Native(_)),
                "{built:?}"
            );
            assert_eq!(compiled(&fixture), "1.0.0\n");
            assert_eq!(
                fs::read_to_string(fixture.root.join("locked")).unwrap(),
                format!("{ID}\n")
            );
            assert!(!source.join("Cargo.lock").exists());
        },
    );
}
