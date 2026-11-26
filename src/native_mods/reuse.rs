//! The no-change fast path (sunrise-org/sunrise-launcher#593): a set of mods built and installed
//! before from the same inputs is found from those inputs alone, before the build is planned, so
//! a Play with nothing changed skips the SDK index, the horizon-mixins pins, the check folder and
//! the SDK tool's check of every mod.
//!
//! A build's version (`version_for`) hashes inputs that are known only after planning (the
//! resolved horizon-mixins revision, the materialized manifests). So the fast path keys a set on
//! what planning starts from instead ([`Inputs`]), and the planned path keeps that key beside the
//! build's `.installed` ([`INPUTS`]) once the build is installed or used again. Only a set whose
//! inputs all match byte for byte finds it; anything else, or a missing or unreadable record,
//! plans the build as before.
use super::*;

/// Beside a build's `.installed`: the [`Inputs`] keys that planned it, one per line.
pub(super) const INPUTS: &str = ".native-inputs";

/// Names this key's layout: a change to what it covers is another key.
const SCHEMA: &str = "native-inputs-v1";

/// How many keys a build keeps (each caller and set of left-out mods that planned it has one).
const KEYS_KEPT: usize = 8;

/// What a build of a set is planned from.
pub(super) struct Inputs<'a> {
    /// The Sunrise commit the build compiles (`build_revision`).
    pub(super) revision: &'a str,
    pub(super) base: &'a str,
    /// The release or Nightly the build starts from.
    pub(super) origin: &'a str,
    pub(super) profile: Profile,
    pub(super) target: Target,
    pub(super) platform: BuildPlatform,
    /// A profile's exact build (`build_exact_client`), which refuses rather than leaves mods out.
    pub(super) exact: bool,
    pub(super) left_out: &'a [LeftOut],
    /// The mods the build keeps: their pins name the SDK indexes that pin horizon-mixins.
    pub(super) mods: &'a [Bundle],
    /// Every requested mod's files ([`files`]).
    pub(super) files: &'a str,
}

impl Inputs<'_> {
    /// Everything planning reads besides the network and the SDK tool, hashed: the launcher (its
    /// check rules), the staging and Rust versions, the profile and target, the revision, base
    /// and platform key, the SDK base, the developer SDK's index, each SDK index kept for a pin,
    /// the mods left out and the mods' files.
    pub(super) fn key(&self) -> String {
        let mut hash = Sha256::new();
        let (cargo, configs) = self.profile.cargo();
        let texts = [
            SCHEMA,
            env!("CARGO_PKG_VERSION"),
            STAGING,
            rust_toolchain::VERSION,
            self.profile.name(),
            cargo,
        ];
        for text in texts {
            part(&mut hash, text.as_bytes());
        }
        part(&mut hash, &(configs.len() as u64).to_le_bytes());
        for text in configs {
            part(&mut hash, text.as_bytes());
        }
        for text in [self.target.name(), self.revision, self.base, self.origin] {
            part(&mut hash, text.as_bytes());
        }
        let developer = crate::developer_install::is_developer(self.origin);
        let platform = platform_key(developer, self.platform, self.origin);
        maybe(&mut hash, platform.as_deref().map(str::as_bytes));
        part(&mut hash, &[u8::from(self.exact)]);
        let sdk_base = std::env::var_os(sdk_cache::BASE_VAR);
        maybe(
            &mut hash,
            sdk_base.as_ref().map(|base| base.as_encoded_bytes()),
        );
        let local = developer_sdk(developer);
        maybe(
            &mut hash,
            local
                .as_ref()
                .map(|local| local.as_os_str().as_encoded_bytes()),
        );
        let local_index =
            local.and_then(|local| fs::read(local.join(sunrise_launcher_sdk::INDEX)).ok());
        maybe(&mut hash, local_index.as_deref());
        let commits: std::collections::BTreeSet<&str> = std::iter::once(self.revision)
            .chain(
                self.mods
                    .iter()
                    .map(|bundle| bundle.native().sunrise.as_str()),
            )
            .collect();
        for commit in commits {
            part(&mut hash, commit.as_bytes());
            maybe(&mut hash, sdk_cache::kept_index(commit).as_deref());
        }
        let mut left: Vec<&str> = self.left_out.iter().map(|left| left.id.as_str()).collect();
        left.sort_unstable();
        part(&mut hash, &(left.len() as u64).to_le_bytes());
        for id in left {
            part(&mut hash, id.as_bytes());
        }
        part(&mut hash, self.files.as_bytes());
        format!("{:x}", hash.finalize())
    }
}

fn part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn maybe(hash: &mut Sha256, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            hash.update([1]);
            part(hash, bytes);
        }
        None => hash.update([0]),
    }
}

/// A digest of `bundles`' files and uses tables, read as [`version_for`] reads them; None when
/// one can't be read (that build is planned, and says why).
pub(super) fn files(bundles: &[Bundle]) -> Option<String> {
    let mut hash = Sha256::new();
    hash_bundles(&mut hash, bundles).ok()?;
    Some(format!("{:x}", hash.finalize()))
}

/// The installed `target` build of Sunrise `revision` whose record holds `key`.
pub(super) fn installed(target: Target, revision: &str, key: &str) -> Option<String> {
    let probe = target.directory(PREFIX);
    let prefix = format!("{PREFIX}{}-", revision.get(..7)?);
    fs::read_dir(probe.parent()?)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(&prefix))
        .find(|name| {
            let dir = target.directory(name);
            dir.join(".installed").is_file()
                && fs::read_to_string(dir.join(INPUTS))
                    .is_ok_and(|keys| keys.lines().any(|line| line == key))
        })
}

/// Keeps `key` in installed build `version`'s record, newest last. A record that can't be written
/// costs only the next build's planning.
pub(super) fn record(target: Target, version: &str, key: &str) {
    let path = target.directory(version).join(INPUTS);
    let mut keys: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty() && *line != key)
        .map(str::to_owned)
        .collect();
    keys.push(key.to_owned());
    let kept = &keys[keys.len().saturating_sub(KEYS_KEPT)..];
    let _ = fs::write(&path, format!("{}\n", kept.join("\n")));
}

/// What `pipeline` looks a build up with: its own arguments.
pub(super) struct Request<'a> {
    pub(super) base: &'a str,
    pub(super) folder: &'a Path,
    pub(super) built_on: Option<&'a About>,
    pub(super) platform: BuildPlatform,
    pub(super) profile: Profile,
    pub(super) target: Target,
    pub(super) options: &'a BuildOptions<'a>,
}

/// An installed build found for a request: its version, and its mods in dependency order.
pub(super) struct Hit {
    pub(super) version: String,
    ordered: Vec<Bundle>,
}

/// The installed build `request` would plan, when its inputs match a record. Anything that
/// would stop the planned build (a duplicate id, a refused dependency, an unknown revision)
/// finds nothing here, so the planned path says it as it always did. A server's build is always
/// planned: its jars are fetched on the way.
pub(super) fn lookup(request: &Request<'_>) -> Option<Hit> {
    let Request {
        base,
        folder,
        built_on,
        platform,
        profile,
        target,
        options,
    } = *request;
    if target != Target::Client {
        return None;
    }
    let found = scan_bundles(folder).ok()?;
    let mut ids = std::collections::HashSet::new();
    if found.is_empty() || !found.iter().all(|b| ids.insert(&b.manifest.id)) {
        return None;
    }
    let side = side_of(target);
    let (left_out, mods, declared) = kept_mods(&found, options.skip, side);
    if mods.is_empty() {
        return None;
    }
    let instance = instance_of(folder, options);
    let overrides =
        dependencies::Overrides::read(&instance.join(sunrise_launcher_smod::mod_list::DIR)).ok()?;
    let resolution =
        resolve_build_dependencies(instance, &mods, &declared, side, &overrides).ok()?;
    let ordered = resolution
        .order
        .iter()
        .map(|&i| mods.get(i).cloned())
        .collect::<Option<Vec<_>>>()?;
    let revision = build_revision(base, built_on).ok()?;
    let files = files(&found)?;
    let key = Inputs {
        revision: &revision,
        base,
        origin: built_on.map_or(base, |about| about.base.as_str()),
        profile,
        target,
        platform,
        exact: options.exact_home.is_some(),
        left_out: &left_out,
        mods: &mods,
        files: &files,
    }
    .key();
    let version = installed(target, &revision, &key)?;
    Some(Hit { version, ordered })
}

/// The found build, used again as the planned path uses an installed one: kept for a staged
/// profile, said to be up to date, and counted as the newest of the builds kept (`prune`).
pub(super) fn reused(
    hit: Hit,
    profile: Profile,
    target: Target,
    options: &BuildOptions<'_>,
    progress: &impl Fn(Step),
    started: std::time::Instant,
) -> Result<Built, Failed> {
    log_internals(&hit.ordered)?;
    if let Some(stage) = options.staging {
        let _publication = crate::run_leases::installation().map_err(cache_error)?;
        stage.retain_target(&hit.version)?;
    }
    timed("Planning the build", started)?;
    options.cancellation.check()?;
    progress(Step::Plan(Plan::UpToDate, profile));
    let _ = fs::write(target.directory(&hit.version).join(".installed"), "");
    Ok(Built {
        left_out: about(&hit.version)
            .map(|about| about.left_out)
            .unwrap_or_default(),
        version: hit.version,
        plan: Plan::UpToDate,
        profile,
        secs: started.elapsed().as_secs(),
    })
}

#[cfg(test)]
mod tests;
