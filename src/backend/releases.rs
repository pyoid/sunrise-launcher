//! Releases: their order, parsing the game's and the launcher's, the SDK offers, and the update check.
use super::{
    asset_installed, compare_game_versions, dev_date, fetch_small, game_assets_for, installation,
    installed, is_game_tag, launcher_asset, native_windows_architecture,
    selected_game_architecture, LauncherCheckError, Release, Settings, ASSET, GAME_FALLBACK,
    GAME_REPO, LAUNCHER_REPO, LAUNCHER_TAG_PREFIX,
};

/// The launcher version in a `vX.Y.Z` tag of [`LAUNCHER_REPO`], or None for any other tag: the verifier's own grammar, so discovery never takes a tag it would refuse.
pub fn launcher_version(tag: &str) -> Option<&str> {
    sunrise_launcher_release_verify::launcher_version(tag)
}

/// The tag this launcher was released as: release.yml gives it (`SUNRISE_LAUNCHER_TAG`); any other build is `v` and its version.
pub fn own_tag() -> String {
    own_tag_of(
        option_env!("SUNRISE_LAUNCHER_TAG"),
        option_env!("SUNRISE_LAUNCHER_VERSION"),
    )
}

pub(super) fn own_tag_of(tag: Option<&str>, injected: Option<&str>) -> String {
    match (
        tag.filter(|t| !t.is_empty()),
        injected.filter(|v| !v.is_empty()),
    ) {
        (Some(tag), _) => tag.to_string(),
        (None, Some(version)) => format!("{LAUNCHER_TAG_PREFIX}{version}"),
        (None, None) => format!("{LAUNCHER_TAG_PREFIX}{}", env!("CARGO_PKG_VERSION")),
    }
}

/// Launcher version ordering, including historical launcher tags: the tags from
/// before 0.2 (`vYYYY.MM.DD[.N]`, `launcher-vX.Y.Z`) and bare versions first, compared number by
/// number with missing parts as zero and anything after a `-` ignored, as launchers always did;
/// then the 0.2 tags (`vX.Y.Z`, `launcher@X.Y.Z`) in semver's order, a pre-release
/// (`v0.3.0-rc.1`) before its release.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReleaseOrder {
    /// Numbers without trailing zeros, so 1.2 and 1.2.0 are equal.
    Before02(Vec<u64>),
    Semver([u64; 3], Prerelease),
}

/// (is a release, the pre-release's identifiers): a release follows every pre-release of its
/// version; identifiers compare one by one, numbers before words, a shorter list first.
pub type Prerelease = (bool, Vec<Identifier>);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Identifier {
    Number(u64),
    Word(String),
}

/// Semver's majors stay below this; a date tag's first number is a year.
const FIRST_YEAR: u64 = 1000;

pub fn release_order(tag: &str) -> ReleaseOrder {
    // The game repository's launcher tags (`launcher@X.Y.Z`, before that `launcher-vX.Y.Z`) still order among the rest.
    let semver = tag
        .strip_prefix("launcher@")
        .or_else(|| tag.strip_prefix(LAUNCHER_TAG_PREFIX))
        .and_then(semver_order);
    semver.unwrap_or_else(|| {
        let v = tag
            .strip_prefix("launcher-v")
            .or_else(|| tag.strip_prefix("launcher@"))
            .unwrap_or(tag);
        let v = v.strip_prefix('v').unwrap_or(v);
        let mut parts: Vec<u64> = v
            .split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        while parts.last() == Some(&0) {
            parts.pop();
        }
        ReleaseOrder::Before02(parts)
    })
}

/// `MAJOR.MINOR.PATCH[-PRE]` with a major below [`FIRST_YEAR`], or None.
fn semver_order(version: &str) -> Option<ReleaseOrder> {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let numbers: Vec<u64> = core
        .split('.')
        .map(|n| n.parse().ok())
        .collect::<Option<_>>()?;
    let &[major, minor, patch] = numbers.as_slice() else {
        return None;
    };
    if major >= FIRST_YEAR {
        return None;
    }
    let pre = match pre {
        None => (true, Vec::new()),
        Some(pre) => (
            false,
            pre.split('.')
                .map(|id| {
                    (!id.is_empty()).then(|| {
                        id.parse()
                            .map_or_else(|_| Identifier::Word(id.to_string()), Identifier::Number)
                    })
                })
                .collect::<Option<_>>()?,
        ),
    };
    Some(ReleaseOrder::Semver([major, minor, patch], pre))
}

/// Compares two tags or versions by [`release_order`].
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    release_order(a).cmp(&release_order(b))
}

/// Every published release of the game, from GitHub or its configured mirror.
pub(super) fn release_list() -> Result<Vec<serde_json::Value>, String> {
    let host = crate::github::API.trim_start_matches("https://");
    let step = crate::game_log::launcher::step(format!("Fetching the release list from {host}"));
    let list = crate::github::release_list(GAME_REPO, crate::github::CachePolicy::PreferCache);
    match &list {
        Ok(list) => step.done(format!("{} releases", list.len())),
        Err(why) => step.failed(why),
    }
    list
}

/// The game's releases in `list` whose tag passes `keep`, with assets in preference order.
pub(super) fn parse_releases(
    list: &[serde_json::Value],
    assets: &[Option<&str>],
    keep: fn(&str) -> bool,
) -> Vec<Release> {
    let mut out: Vec<_> = parse_releases_from(list, assets, keep, crate::signed::asset_url)
        .into_iter()
        .map(|release| {
            // More than one Dev build can be published on a build day. The abbreviated
            // commit disambiguates them, but does not order them. Parse once so mixed UTC
            // offsets compare by instant and invalid timestamps stay consistently last
            // within the build day.
            let published = dev_date(&release.tag).and_then(|_| {
                chrono::DateTime::parse_from_rfc3339(&release.published_at)
                    .ok()
                    .map(chrono::DateTime::<chrono::Utc>::from)
            });
            (release, published)
        })
        .collect();
    out.sort_by(|(a, a_published), (b, b_published)| {
        compare_game_versions(&b.tag, &a.tag).then_with(|| b_published.cmp(a_published))
    });
    out.into_iter().map(|(release, _)| release).collect()
}

/// This launcher's releases in `list` (from [`LAUNCHER_REPO`]), newest first, with the asset named `asset`, downloaded from there.
pub(crate) fn parse_launcher_releases(
    list: &[serde_json::Value],
    asset: Option<&str>,
) -> Vec<Release> {
    let mut out = parse_releases_from(
        list,
        &[asset],
        is_launcher_tag,
        crate::signed::launcher_asset_url,
    );
    out.sort_by(|a, b| compare_versions(&b.tag, &a.tag));
    out
}

fn parse_releases_from(
    list: &[serde_json::Value],
    assets: &[Option<&str>],
    keep: fn(&str) -> bool,
    url: fn(&str, &str) -> String,
) -> Vec<Release> {
    let mut out = Vec::new();
    for r in list {
        let tag = r["tag_name"].as_str().unwrap_or_default();
        if r["draft"].as_bool().unwrap_or(false) || !keep(tag) {
            continue;
        }
        let found = assets.iter().flatten().find_map(|name| {
            r["assets"].as_array()?.iter().find_map(|asset| {
                (asset["name"].as_str()? == *name)
                    .then(|| Some((url(tag, name), asset["size"].as_u64()?)))?
            })
        });
        let published = r["published_at"].as_str().unwrap_or_default();
        out.push(Release {
            tag: tag.into(),
            name: r["name"].as_str().unwrap_or_default().into(),
            body: r["body"].as_str().unwrap_or_default().replace("\r\n", "\n"),
            date: published.chars().take(10).collect(),
            time: published.get(11..16).unwrap_or_default().into(),
            published_at: published.into(),
            asset: found,
            prerelease: r["prerelease"].as_bool().unwrap_or(false),
            nightly: None,
        });
    }
    out
}

fn is_launcher_tag(tag: &str) -> bool {
    launcher_version(tag).is_some()
}

/// Published Sunrise builds: the current Nightly, then Dev and Stable builds, newest first
/// within each channel. Dated Nightlies and launcher releases aren't offered here.
pub fn releases() -> Result<Vec<Release>, String> {
    Ok(game_releases(&release_list()?))
}

/// The game's builds in `list`, newest first. A Nightly is listed under the version it installs as
/// ([`crate::nightly::version_of`]); only the current one (`nightly`) is an update.
pub(crate) fn game_releases(list: &[serde_json::Value]) -> Vec<Release> {
    game_releases_from(
        list,
        &Settings::load(),
        &crate::native_mods::sdk_cache::Base::from_env(),
        &sunrise_launcher_release_verify::trusted_keys_for(GAME_REPO),
    )
}

pub(super) fn game_releases_from(
    list: &[serde_json::Value],
    settings: &Settings,
    base: &crate::native_mods::sdk_cache::Base,
    keys: &[sunrise_launcher_release_verify::PublicKey],
) -> Vec<Release> {
    let (mut releases, target) = game_releases_candidates(list, settings);
    if let Some(target) = target {
        check_sdk_offers(&mut releases, target, base, keys);
    }
    releases
}

pub(super) fn game_releases_candidates(
    list: &[serde_json::Value],
    settings: &Settings,
) -> (Vec<Release>, Option<&'static str>) {
    let assets = game_assets_for(
        std::env::consts::OS,
        native_windows_architecture(),
        std::env::consts::ARCH,
        settings.game_architecture,
    );
    let (asset, fallback) = assets.map_or((ASSET, GAME_FALLBACK), |(asset, fallback)| {
        (Some(asset), fallback)
    });
    let target = installation::sdk_target(selected_game_architecture(settings));
    let releases = game_releases_with(
        list,
        &[asset, fallback, target.map(|_| sunrise_launcher_sdk::INDEX)],
    );
    (releases, target)
}

pub(super) fn is_sdk_candidate(release: &Release) -> bool {
    asset_installed(release, Some(sunrise_launcher_sdk::INDEX), None).is_some()
}

/// Strip unverified SDK-only candidates so known static builds can be published immediately.
fn static_only_offers(candidates: &[Release]) -> Vec<Release> {
    candidates
        .iter()
        .cloned()
        .map(|mut release| {
            if is_sdk_candidate(&release) {
                release.asset = None;
            }
            release
        })
        .collect()
}

/// Verify all SDK-only candidates in `releases` (in parallel when there are multiple) and return
/// whether at least one candidate remained offered.
fn check_sdk_offers(
    releases: &mut [Release],
    target: &str,
    base: &crate::native_mods::sdk_cache::Base,
    keys: &[sunrise_launcher_release_verify::PublicKey],
) -> bool {
    let mut sdk_releases: Vec<&mut Release> = releases
        .iter_mut()
        .filter(|release| is_sdk_candidate(release))
        .collect();
    if sdk_releases.is_empty() {
        return false;
    }
    if sdk_releases.len() == 1 {
        check_sdk_offer(sdk_releases[0], target, base, keys);
    } else {
        std::thread::scope(|scope| {
            for release in &mut sdk_releases {
                scope.spawn(|| check_sdk_offer(release, target, base, keys));
            }
        });
    }
    sdk_releases.iter().any(|release| release.asset.is_some())
}

/// Only SDK candidates need an index lookup; static archives take precedence without one.
fn check_sdk_offer(
    release: &mut Release,
    target: &str,
    base: &crate::native_mods::sdk_cache::Base,
    keys: &[sunrise_launcher_release_verify::PublicKey],
) {
    if is_sdk_candidate(release)
        && !crate::native_mods::sdk_cache::offers_game(
            &release.tag,
            release.nightly.as_deref().unwrap_or(&release.tag),
            target,
            base,
            keys,
        )
    {
        release.asset = None;
    }
}

/// Like [`check_sdk_offer`], but preserves lookup/verification errors for explicit installs instead
/// of turning them into a missing-build message.
pub(super) fn check_sdk_offer_strict(
    release: &mut Release,
    target: &str,
    base: &crate::native_mods::sdk_cache::Base,
    keys: &[sunrise_launcher_release_verify::PublicKey],
) -> Result<(), String> {
    if is_sdk_candidate(release) {
        match crate::native_mods::sdk_cache::check_game_offer(
            &release.tag,
            release.nightly.as_deref().unwrap_or(&release.tag),
            target,
            base,
            keys,
        ) {
            Ok(true) => {}
            Ok(false) => release.asset = None,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Parse game releases with explicit asset preferences; SDK candidates are checked after parsing.
pub(super) fn game_releases_with(
    list: &[serde_json::Value],
    assets: &[Option<&str>],
) -> Vec<Release> {
    use sunrise_launcher_release_verify::{build_title_version, Channel, NIGHTLY};
    let keep = |tag: &str| tag == NIGHTLY || dev_date(tag).is_some() || is_game_tag(tag);
    let mut out = parse_releases(list, assets, keep);
    out.retain_mut(|r| match Channel::of(&r.tag) {
        Some(Channel::Nightly) => {
            let version = crate::nightly::version_of(r);
            let tag = std::mem::replace(&mut r.tag, version);
            r.prerelease = false;
            r.nightly = Some(tag);
            true
        }
        Some(Channel::Dev) => {
            if !r.prerelease || build_title_version(Channel::Dev, &r.name).as_ref() != Some(&r.tag)
            {
                return false;
            }
            r.nightly = Some(r.tag.clone());
            true
        }
        None => !r.prerelease,
    });
    out
}

/// Check the launcher's own release feed. An explicit check must ask GitHub even if the
/// cache is fresh, and must not report old cached results as a successful check.
pub fn refresh_launcher_update() -> Result<Option<Release>, LauncherCheckError> {
    checked_launcher_update(crate::github::CachePolicy::Revalidate)
}

/// Advertise a newer launcher only when its manifest was signed for this tag and platform.
fn checked_launcher_update(
    policy: crate::github::CachePolicy,
) -> Result<Option<Release>, LauncherCheckError> {
    let list = crate::github::release_list(LAUNCHER_REPO, policy)
        .map_err(LauncherCheckError::Unavailable)?;
    let Some(asset) = launcher_asset() else {
        return Ok(None);
    };
    signed_launcher_offer(
        &list,
        asset,
        &sunrise_launcher_release_verify::trusted_launcher_keys(),
        fetch_small,
    )
}

pub(super) fn signed_launcher_offer(
    list: &[serde_json::Value],
    asset: &str,
    keys: &[sunrise_launcher_release_verify::PublicKey],
    fetch: impl Fn(&str) -> Result<Option<Vec<u8>>, String>,
) -> Result<Option<Release>, LauncherCheckError> {
    let Some(release) = launcher_update(&parse_launcher_releases(list, Some(asset))) else {
        return Ok(None);
    };
    crate::signed::launcher_digest(&release.tag, asset, keys, fetch)?;
    Ok(Some(release))
}

/// The newest Sunrise release that has a build for this platform.
pub fn latest(releases: &[Release]) -> Option<&Release> {
    releases.iter().find(|r| r.asset.is_some())
}

/// How often a launcher left open looks for new releases: they come out several times a day.
pub const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// What a check found: a newer Sunrise build and a newer launcher, each feed checked on its own, with why one couldn't be read.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Updates {
    pub sunrise: Option<Release>,
    pub launcher: Option<Release>,
    pub sunrise_error: Option<String>,
    pub launcher_error: Option<LauncherCheckError>,
}

/// An update stays on the profile's followed channel. Installed builds on another channel
/// do not suppress it. Another build published on the same day can still be an update.
pub fn sunrise_update<'a>(
    releases: &'a [Release],
    installed: &[String],
    channel: crate::instances::Channel,
) -> Option<&'a Release> {
    let newest = channel.builds(releases).next()?;
    let newer = !installed.contains(&newest.tag)
        && crate::instances::newest_installed(channel, installed).is_none_or(|tag| {
            let order = compare_game_versions(&newest.tag, tag);
            match channel {
                crate::instances::Channel::Stable => order.is_gt(),
                crate::instances::Channel::Nightly | crate::instances::Channel::Dev => {
                    order.is_ge()
                }
            }
        });
    newer.then_some(newest)
}

/// The newest launcher release with an archive for this platform, if newer than this launcher ([`own_tag`]).
pub fn launcher_update(releases: &[Release]) -> Option<Release> {
    launcher_update_from(releases, &own_tag())
}

pub(super) fn launcher_update_from(releases: &[Release], own: &str) -> Option<Release> {
    releases
        .iter()
        .find(|r| r.asset.is_some())
        .filter(|r| compare_versions(&r.tag, own).is_gt())
        .cloned()
}

/// Both release lists and both update kinds: static game releases published before launcher
/// updates and SDK lookups, and neither feed's failure stopping the other's check.
pub fn releases_and_updates(on_releases: impl FnMut(Result<Vec<Release>, String>)) -> Updates {
    let step = crate::game_log::launcher::step("Checking for updates");
    let updates = releases_and_updates_with(
        release_list,
        installed,
        crate::instances::selection()
            .ok()
            .and_then(|instance| instance.version.is_none().then_some(instance.channel)),
        || checked_launcher_update(crate::github::CachePolicy::PreferCache),
        on_releases,
    );
    let newest = |release: &Option<Release>| {
        release
            .as_ref()
            .map_or_else(|| "none".to_owned(), |r| r.tag.clone())
    };
    let errors: Vec<String> = [
        updates.sunrise_error.clone(),
        updates.launcher_error.as_ref().map(ToString::to_string),
    ]
    .into_iter()
    .flatten()
    .collect();
    if errors.is_empty() {
        step.done(format!(
            "game update: {}; launcher update: {}",
            newest(&updates.sunrise),
            newest(&updates.launcher)
        ));
    } else {
        step.failed(errors.join("; "));
    }
    updates
}

pub(super) fn releases_and_updates_with(
    load: impl FnOnce() -> Result<Vec<serde_json::Value>, String>,
    installed: impl FnOnce() -> Vec<String>,
    channel: Option<crate::instances::Channel>,
    launcher: impl FnOnce() -> Result<Option<Release>, LauncherCheckError>,
    on_releases: impl FnMut(Result<Vec<Release>, String>),
) -> Updates {
    releases_and_updates_with_sdk(
        load,
        installed,
        channel,
        launcher,
        on_releases,
        |candidates, target| {
            let base = crate::native_mods::sdk_cache::Base::from_env();
            let keys = sunrise_launcher_release_verify::trusted_keys_for(GAME_REPO);
            check_sdk_offers(candidates, target, &base, &keys)
        },
    )
}

pub(super) fn releases_and_updates_with_sdk(
    load: impl FnOnce() -> Result<Vec<serde_json::Value>, String>,
    installed: impl FnOnce() -> Vec<String>,
    channel: Option<crate::instances::Channel>,
    launcher: impl FnOnce() -> Result<Option<Release>, LauncherCheckError>,
    mut on_releases: impl FnMut(Result<Vec<Release>, String>),
    resolve_sdk: impl FnOnce(&mut [Release], &str) -> bool,
) -> Updates {
    let mut updates = Updates::default();
    let mut pending_sdk = None;
    let mut installed_versions = None;
    match load() {
        Ok(list) => {
            let settings = Settings::load();
            let (candidates, target) = game_releases_candidates(&list, &settings);
            let has_sdk = target.is_some() && candidates.iter().any(is_sdk_candidate);
            let static_offers = if has_sdk {
                static_only_offers(&candidates)
            } else {
                candidates.clone()
            };
            let versions = installed();
            updates.sunrise = channel
                .and_then(|c| sunrise_update(&static_offers, &versions, c))
                .cloned();
            on_releases(Ok(static_offers));
            if let Some(target) = target.filter(|_| has_sdk) {
                pending_sdk = Some((candidates, target));
                installed_versions = Some(versions);
            }
        }
        Err(e) => {
            on_releases(Err(e.clone()));
            updates.sunrise_error = Some(e);
        }
    }
    match launcher() {
        Ok(release) => updates.launcher = release,
        Err(e) => updates.launcher_error = Some(e),
    }
    if let (Some((mut candidates, target)), Some(versions)) = (pending_sdk, installed_versions) {
        if resolve_sdk(&mut candidates, target) {
            updates.sunrise = channel
                .and_then(|c| sunrise_update(&candidates, &versions, c))
                .cloned();
            on_releases(Ok(candidates));
        }
    }
    updates
}

/// A fresh release-list request for both kinds of update (plus older-launcher fallback if needed).
pub fn check_updates() -> Updates {
    releases_and_updates(|_| {})
}
