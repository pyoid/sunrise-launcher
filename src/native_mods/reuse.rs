//! The no-change fast path (sunrise-org/sunrise-launcher#593): a set of mods built and installed
//! before is found again before its SDK check, so a Play with nothing changed skips the SDK tool's
//! check of every mod.
//!
//! The installed build is named by its version ([`version_for`]), and that is the one identity
//! the lookup trusts. From a private copy of the mods (a stable view of them), it works the
//! version out as planning does, but from the SDK indexes already kept on disk and nothing
//! fetched, and then proves the build installed under it is the one that version names:
//!
//! - the build's [`EVIDENCE`], written by the planned path from its verified plan once the build
//!   was installed or used again, holds the plan's inputs that change the build without being in
//!   its version ([`Image`]: the pins, the Cargo and dispatch orders, the mods left out and the
//!   digest of each SDK index read). They must be the ones the lookup works out.
//! - The rules that admitted the plan without changing the build ([`Admission`]: the launcher's
//!   own checks, the base, an exact profile build, the SDK base) must be the lookup's.
//! - The build's record (`native-mods.json`) and its installed mods must be the plan's
//!   ([`Contents`]).
//! - The installed game must still be the one the evidence sealed ([`Artifact`]: the executable
//!   and every file it loads beside itself).
//!
//! Then the profile is read again, and the mods' folder is compared with the copy: a change while
//! the build was proved misses. Anything else, or a missing, torn or unknown evidence file, plans
//! the build as before.
use super::*;
use crate::managed_play::Artifact;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Beside an installed client build: what the fast path proves it with ([`Evidence`]).
pub(super) const EVIDENCE: &str = ".native-reuse.json";

/// The evidence's layout. Another layout is no evidence: the build is planned, and the plan
/// writes it again.
const SCHEMA: u32 = 1;

/// How many admissions one build keeps (each caller and base that planned it has one).
const ADMISSIONS_KEPT: usize = 8;

/// What a build used beyond its version: the version hashes the revision, the horizon-mixins
/// key, the profile, the target, the platform key, the requested mods' files and the mods left
/// out, and nothing of these.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    /// Each kept mod's horizon-mixins pin, by id: what its copy's manifest was given.
    pins: BTreeMap<String, String>,
    /// The kept mods in Cargo link order: the order the build tool was given them in.
    link: Vec<String>,
    /// The kept mods in the dispatch order the instance's overrides allow.
    dispatch: Vec<String>,
    /// The mods left out, sorted.
    left_out: Vec<String>,
    /// The SHA-256 of each signed SDK index the plan read, by build commit.
    sdk: BTreeMap<String, String>,
    /// The SHA-256 of the developer SDK's index, when a pin was read from it.
    local_sdk: Option<String>,
}

impl Image {
    /// `mods` (the kept mods, with their `pins`), in `link` order and `dispatch` order, with
    /// `left_out` left out and the indexes `read`.
    pub(super) fn new(
        mods: &[Bundle],
        pins: &[String],
        link: &[Bundle],
        dispatch: &[String],
        left_out: &[LeftOut],
        read: &mixins::Read,
    ) -> Self {
        let mut left: Vec<String> = left_out.iter().map(|left| left.id.clone()).collect();
        left.sort_unstable();
        Self {
            pins: mods
                .iter()
                .zip(pins)
                .map(|(bundle, pin)| (bundle.manifest.id.clone(), pin.clone()))
                .collect(),
            link: link
                .iter()
                .map(|bundle| bundle.manifest.id.clone())
                .collect(),
            dispatch: dispatch.to_vec(),
            left_out: left,
            sdk: read.sdk.clone(),
            local_sdk: read.local.clone(),
        }
    }
}

/// What admitted a plan without changing what it built: the launcher whose checks passed it,
/// the version it started from, whether it was a profile's exact build, and where SDK indexes
/// come from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Admission {
    launcher: String,
    base: String,
    origin: String,
    exact: bool,
    /// A fingerprint of [`sdk_cache::BASE_VAR`] ([`sdk_base_fingerprint`]), never the setting:
    /// a mirror's address can carry its credentials.
    sdk_base: Option<String>,
}

impl Admission {
    pub(super) fn new(base: &str, origin: &str, exact: bool) -> Self {
        Self::with_sdk_base(
            base,
            origin,
            exact,
            std::env::var_os(sdk_cache::BASE_VAR).as_deref(),
        )
    }

    /// [`Self::new`], with `sdk_base` as the SDK base setting.
    pub(super) fn with_sdk_base(
        base: &str,
        origin: &str,
        exact: bool,
        sdk_base: Option<&std::ffi::OsStr>,
    ) -> Self {
        Self {
            launcher: backend::VERSION.to_owned(),
            base: base.to_owned(),
            origin: origin.to_owned(),
            exact,
            sdk_base: sdk_base.map(sdk_base_fingerprint),
        }
    }
}

/// What the evidence keeps of the SDK base setting: enough to tell another setting from this
/// one, as #622's key hashed it, and nothing to read it back from.
fn sdk_base_fingerprint(setting: &std::ffi::OsStr) -> String {
    let mut hash = Sha256::new();
    hash.update(b"sunrise-launcher sdk base\0");
    hash.update(setting.as_encoded_bytes());
    format!("{:x}", hash.finalize())
}

/// The file beside a build ([`EVIDENCE`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    schema: u32,
    version: String,
    image: Image,
    /// The installed game when the evidence was written.
    seal: Artifact,
    /// Newest last.
    admitted: Vec<Admission>,
}

impl Evidence {
    /// `dir`'s evidence; None when there is none, or it is torn or in another layout.
    fn read(dir: &Path) -> Option<Self> {
        let evidence: Self = serde_json::from_slice(&fs::read(dir.join(EVIDENCE)).ok()?).ok()?;
        (evidence.schema == SCHEMA).then_some(evidence)
    }
}

/// What an installed build of a plan holds: its record and its mods.
pub(super) struct Contents<'a> {
    pub(super) revision: &'a str,
    pub(super) origin: &'a str,
    pub(super) mixins: &'a str,
    pub(super) profile: Profile,
    /// The kept mods, with each one's source digest in `digests`.
    pub(super) kept: &'a [Bundle],
    pub(super) digests: &'a HashMap<String, Option<String>>,
    pub(super) left_out: &'a [LeftOut],
}

impl Contents<'_> {
    /// Whether installed client build `version` is this: its record names the revision, the
    /// release it started from, the horizon-mixins key, the profile and the mods left out, it
    /// includes exactly the kept mods at their versions and digests (as Play checks them,
    /// `native_status::included`), and holds exactly their copies.
    fn installed_in(&self, version: &str) -> bool {
        let dir = Target::Client.directory(version);
        let Ok(Some(about)) = checked_about(version) else {
            return false;
        };
        let mut left: Vec<&str> = about.left_out.iter().map(|l| l.id.as_str()).collect();
        let mut planned: Vec<&str> = self.left_out.iter().map(|l| l.id.as_str()).collect();
        left.sort_unstable();
        planned.sort_unstable();
        if about.target != Target::Client
            || about.sunrise != self.revision
            || about.base != self.origin
            || left != planned
        {
            return false;
        }
        let Some(record) = fs::read(dir.join(RECORD))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            return false;
        };
        if record["horizon_mixins"].as_str() != Some(self.mixins)
            || record["profile"].as_str() != Some(self.profile.name())
        {
            return false;
        }
        let Some(expected) = self
            .kept
            .iter()
            .map(|bundle| {
                let digest = self.digests.get(&bundle.manifest.id)?.as_ref()?;
                let id = bundle.manifest.id.clone();
                Some((
                    id,
                    bundle.manifest.version.clone(),
                    digest.to_ascii_lowercase(),
                ))
            })
            .collect::<Option<BTreeSet<_>>>()
        else {
            return false;
        };
        let Ok(included) = crate::native_status::included(version) else {
            return false;
        };
        let held: BTreeSet<_> = included
            .into_iter()
            .map(|m| (m.id, m.version, m.sha256.to_ascii_lowercase()))
            .collect();
        let copies: Option<BTreeSet<String>> =
            fs::read_dir(dir.join(sunrise_launcher_smod::mod_list::NATIVE_DIR))
                .ok()
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|entry| entry.file_name().into_string().ok())
                        .collect()
                });
        let kept: BTreeSet<String> = self.kept.iter().map(|b| b.manifest.id.clone()).collect();
        held == expected && copies.as_ref() == Some(&kept)
    }
}

/// How many versions a plan tries ([`versions`]) before it gives up.
const VERSIONS_TRIED: usize = 8;

/// The versions a client plan installs as, in order: its own (`version_for`), then one after
/// another while a build under the one before is installed but doesn't hold the plan
/// ([`holds`]): a build an older launcher made without some of the mods under the whole set's
/// version, or one whose game changed after it was sealed. Such a build is never used for the
/// plan, nor sealed again, nor replaced while something may still run it: the plan installs
/// under the next version, the same one every time for the same plan.
pub(super) fn versions(version: &str) -> impl Iterator<Item = String> + '_ {
    (0..VERSIONS_TRIED).map(move |n| {
        if n == 0 {
            return version.to_owned();
        }
        let mut hash = Sha256::new();
        hash.update(b"in place of\0");
        hash.update(version.as_bytes());
        hash.update((n as u64).to_le_bytes());
        let key: String = hash.finalize()[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let head = version
            .get(..version.len().saturating_sub(12))
            .unwrap_or(version);
        format!("{head}{key}")
    })
}

/// Whether installed client build `version` holds the plan `contents` ([`Contents`]) and is still
/// the game its evidence sealed. A build with no evidence yet (one an older launcher made) has
/// no seal to break: it is sealed when it is first used. Evidence that can't be read is no
/// such build: it is never sealed again.
pub(super) fn holds(version: &str, contents: &Contents<'_>) -> bool {
    let dir = Target::Client.directory(version);
    contents.installed_in(version)
        && match sealed(&dir) {
            Sealed::None => true,
            Sealed::Unreadable => false,
            Sealed::As(evidence) => {
                evidence.version == version
                    && Artifact::capture_at(version, &dir).is_ok_and(|seal| seal == evidence.seal)
            }
        }
}

/// The version client plan `version` installs as ([`versions`]), and whether a build there
/// already holds it ([`holds`]).
pub(super) fn installable(
    version: &str,
    contents: &Contents<'_>,
) -> Result<(String, bool), Failed> {
    for candidate in versions(version) {
        if !installed(&candidate) {
            return Ok((candidate, false));
        }
        if holds(&candidate, contents) {
            return Ok((candidate, true));
        }
    }
    Err(Failed::Other(trf(
        "Every installed build named for these mods ({version}) holds other mods or was changed after it was installed. Remove them, then build again.",
        &[("version", version)],
    )))
}

fn installed(version: &str) -> bool {
    Target::Client
        .directory(version)
        .join(".installed")
        .is_file()
}

/// A build's evidence, as [`holds`] and [`record`] read it.
enum Sealed {
    None,
    Unreadable,
    As(Box<Evidence>),
}

fn sealed(dir: &Path) -> Sealed {
    match fs::read(dir.join(EVIDENCE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Sealed::None,
        Err(_) => Sealed::Unreadable,
        Ok(bytes) => serde_json::from_slice::<Evidence>(&bytes)
            .ok()
            .filter(|evidence| evidence.schema == SCHEMA)
            .map_or(Sealed::Unreadable, |evidence| {
                Sealed::As(Box::new(evidence))
            }),
    }
}

/// A planned client build, once it is installed or used again.
pub(super) struct Planned<'a> {
    pub(super) version: &'a str,
    pub(super) image: Image,
    pub(super) admission: Admission,
    pub(super) contents: &'a Contents<'a>,
}

/// Keeps `planned`'s evidence beside its build, when the installed build is the one planned
/// ([`Contents`]); else nothing is kept, so the build isn't found without planning. A build is
/// sealed once: when it is published, or first used when an older launcher made it. Evidence
/// whose seal the game no longer matches, or that can't be read, is left as it is: the changed
/// game is never sealed. A failure costs only the next build's planning.
pub(super) fn record(planned: &Planned<'_>) {
    let dir = Target::Client.directory(planned.version);
    if !planned.contents.installed_in(planned.version) {
        return;
    }
    let Ok(seal) = Artifact::capture_at(planned.version, &dir) else {
        return;
    };
    let old = match sealed(&dir) {
        Sealed::None => None,
        Sealed::As(old) if old.version == planned.version && old.seal == seal => Some(old),
        Sealed::As(_) | Sealed::Unreadable => return,
    };
    let mut admitted = old
        .filter(|old| old.image == planned.image)
        .map(|old| old.admitted)
        .unwrap_or_default();
    admitted.retain(|admission| *admission != planned.admission);
    admitted.push(planned.admission.clone());
    let drop = admitted.len().saturating_sub(ADMISSIONS_KEPT);
    admitted.drain(..drop);
    let evidence = Evidence {
        schema: SCHEMA,
        version: planned.version.to_owned(),
        image: planned.image.clone(),
        seal,
        admitted,
    };
    if let Ok(bytes) = serde_json::to_vec(&evidence) {
        let _ = sdk_cache::write_whole(&dir.join(EVIDENCE), &bytes);
    }
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

/// What planning would make of a request, worked out without fetching or checking anything.
#[derive(Debug, PartialEq)]
struct Derived {
    version: String,
    image: Image,
    admission: Admission,
    revision: String,
    origin: String,
    mixins: String,
    mods: Vec<Bundle>,
    ordered: Vec<Bundle>,
    left_out: Vec<LeftOut>,
}

/// The pin of each of `mods` from the SDK indexes kept on disk only.
fn kept_pins(
    mods: &[Bundle],
    build_sdk: Option<&sdk_cache::Sdk>,
    local: Option<&Path>,
) -> Option<(Vec<String>, mixins::Read)> {
    mixins::read_pins(mods, build_sdk, local, |pin| {
        sdk_cache::kept(pin).ok_or_else(|| Failed::Other(String::new()))
    })
    .ok()
}

/// `request`'s version and plan inputs, as `pipeline` works them out, from `requested` (the
/// mods' private copies, which take their pins as planning gives them). None when planning
/// would need anything not on disk, or would stop.
fn derive(request: &Request<'_>, requested: &[Bundle]) -> Option<Derived> {
    let Request {
        base,
        folder,
        built_on,
        platform,
        profile,
        target,
        options,
    } = *request;
    let side = side_of(target);
    let (left_out, mods, declared) = kept_mods(requested, options.skip, side);
    // A profile's exact build never installs a set with mods left out.
    if mods.is_empty() || (options.exact_home.is_some() && !left_out.is_empty()) {
        return None;
    }
    let instance = instance_of(folder, options);
    let overrides =
        dependencies::Overrides::read(&instance.join(sunrise_launcher_smod::mod_list::DIR)).ok()?;
    let (dispatch, links) =
        resolve_build_dependencies(instance, &mods, &declared, side, &overrides).ok()?;
    let ordered = links
        .order
        .iter()
        .map(|&i| mods.get(i).cloned())
        .collect::<Option<Vec<_>>>()?;
    let revision = build_revision(base, built_on).ok()?;
    let origin = built_on.map_or(base, |about| about.base.as_str());
    let developer = crate::developer_install::is_developer(origin);
    let sdk = match developer {
        true if mismatch(&mods, &revision).is_some() => return None,
        true => None,
        false => Some(sdk_cache::kept(&revision)?),
    };
    let (pins, mut read) = kept_pins(&mods, sdk.as_ref(), developer_sdk(developer).as_deref())?;
    if let Some(sdk) = &sdk {
        read.sdk.insert(revision.clone(), sdk.signed().to_owned());
    }
    for (bundle, pin) in mods.iter().zip(&pins) {
        mixins::materialize(bundle, &bundle.dir, pin).ok()?;
    }
    let mixins = mixins::for_key(&pins, |pin| {
        remembered(
            &root().join(DATES),
            "horizon-rust/horizon-mixins",
            pin,
            |_, _| Err(String::new()),
        )
    })
    .ok()?;
    let version = version_for(
        &revision,
        &mixins,
        profile,
        target,
        platform_key(developer, platform, origin).as_deref(),
        requested,
        &left_out,
    )
    .ok()?;
    Some(Derived {
        image: Image::new(&mods, &pins, &ordered, &dispatch, &left_out, &read),
        admission: Admission::new(base, origin, options.exact_home.is_some()),
        version,
        revision,
        origin: origin.to_owned(),
        mixins,
        mods,
        ordered,
        left_out,
    })
}

/// An installed build found for a request: its version, and its mods in dependency order.
#[derive(Debug)]
pub(super) struct Hit {
    pub(super) version: String,
    ordered: Vec<Bundle>,
}

/// The installed client build `request` would plan, proved as the module says, from
/// `requested`: private copies of `request`'s mods not yet given their pins.
pub(super) fn lookup(request: &Request<'_>, requested: &[Bundle]) -> Option<Hit> {
    if request.target != Target::Client {
        return None;
    }
    let digests: HashMap<String, Option<String>> = requested
        .iter()
        .map(|bundle| (bundle.manifest.id.clone(), source_digest(bundle)))
        .collect();
    let derived = derive(request, requested)?;
    let contents = Contents {
        revision: &derived.revision,
        origin: &derived.origin,
        mixins: &derived.mixins,
        profile: request.profile,
        kept: &derived.mods,
        digests: &digests,
        left_out: &derived.left_out,
    };
    // The version `installable` would take, when a build there holds the plan: then proved.
    let version = versions(&derived.version)
        .take_while(|version| installed(version))
        .find(|version| holds(version, &contents))?;
    let proved = Evidence::read(&request.target.directory(&version)).is_some_and(|evidence| {
        evidence.version == version
            && evidence.image == derived.image
            && evidence.admitted.contains(&derived.admission)
    });
    // Read again: a profile, override or kept index that changed meanwhile, or a mod changed in
    // the folder since it was copied, is another build.
    let unchanged = || {
        derive(request, requested).as_ref() == Some(&derived)
            && scan_bundles(request.folder).is_ok_and(|found| {
                found.len() == digests.len()
                    && found.iter().all(|bundle| {
                        digests
                            .get(&bundle.manifest.id)
                            .is_some_and(|digest| *digest == source_digest(bundle))
                    })
            })
    };
    (proved && unchanged()).then(|| Hit {
        version,
        ordered: derived.ordered,
    })
}

/// [`lookup`], timed in the build log, and the build it finds used again ([`reused`]).
pub(super) fn find(
    request: &Request<'_>,
    requested: &[Bundle],
    progress: &impl Fn(Step),
    started: std::time::Instant,
) -> Result<Option<Built>, Failed> {
    if request.target != Target::Client {
        return Ok(None);
    }
    let looking = std::time::Instant::now();
    let hit = lookup(request, requested);
    timed("Looking up the installed build", looking)?;
    hit.map(|hit| {
        reused(
            hit,
            request.profile,
            request.target,
            request.options,
            progress,
            started,
        )
    })
    .transpose()
}

/// [`find`] for a build of `folder`'s mods on `base`, before anything of the build is planned or
/// checked: what [`build`] tries before it checks the mods to choose a profile. Whatever stops
/// this (an unprepared base, a duplicate mod) finds nothing, and the build then says it as before.
pub(super) fn before_planning(
    base: &str,
    folder: &Path,
    profile: Profile,
    options: &BuildOptions<'_>,
    progress: &impl Fn(Step),
) -> Result<Option<Built>, Failed> {
    let started = std::time::Instant::now();
    let (Ok(built_on), Ok(()), Ok(platform)) = (
        checked_about(base),
        backend::need_prepared(base),
        platform_for_game(base),
    ) else {
        return Ok(None);
    };
    let Ok(found) = scan_bundles(folder) else {
        return Ok(None);
    };
    let mut ids = std::collections::HashSet::new();
    if found.is_empty() || !found.iter().all(|b| ids.insert(&b.manifest.id)) {
        return Ok(None);
    }
    fs::create_dir_all(root()).map_err(|e| e.to_string())?;
    let copies = tempfile::Builder::new()
        .prefix(".native-input-")
        .tempdir_in(root())
        .map_err(|e| e.to_string())?;
    let mut requested = Vec::new();
    for bundle in found {
        let dir = copies.path().join(&bundle.manifest.id);
        if copy_bundle(&bundle, &dir).is_err() {
            return Ok(None);
        }
        requested.push(Bundle { dir, ..bundle });
    }
    let request = Request {
        base,
        folder,
        built_on: built_on.as_ref(),
        platform,
        profile,
        target: Target::Client,
        options,
    };
    find(&request, &requested, progress, started)
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
