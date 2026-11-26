//! The outside addresses the launcher has built in, vanilla's aside, in one place.

/// The signed v2 mod registry's index, published by sunrise-org/sunrise-mods
/// (`SUNRISE_MOD_INDEX_V2` overrides it).
pub const MOD_INDEX_V2: &str = "https://sunrise-org.github.io/sunrise-mods/v2/index.signed";

/// The registry's copy of the game repository's GitHub API reads, used when GitHub rate-limits a
/// network: `/repos/sunrise-org/sunrise/releases` is at `<this>/repos/sunrise-org/sunrise/releases`
/// (`SUNRISE_GITHUB_MIRROR` overrides it).
pub const GITHUB_MIRROR: &str = "https://sunrise-org.github.io/sunrise-mods/github";

/// The registry's Nightly source listings. A listing does not vet the source or authorize code.
pub const MOD_SOURCES: &str = "https://sunrise-org.github.io/sunrise-mods/sources.json";

/// A registry project's page on the website is `<this>/<slug>`.
pub const MOD_PAGES: &str = "https://playsunrise.net/mods";

/// Where the registry's icons and screenshots are served from: the same GitHub Pages site.
pub const MOD_IMAGES: &str = "https://sunrise-org.github.io/sunrise-mods/";

/// The registry service's API, through the website: ratings (`/ratings`, `/<id>/rating`).
pub const MOD_API: &str = "https://playsunrise.net/api/mods";

/// The telemetry ingest server (`crates/sunrise-telemetry-server` in the game repository): opt-in
/// telemetry, the "I'm playing" ping, benchmark uploads (`SUNRISE_TELEMETRY_URL` overrides it).
pub const TELEMETRY: &str = "https://telemetry.playsunrise.net";

/// The GitHub profile of who made the launcher's look, credited in Settings' About (its picture is
/// `<this>.png`).
pub const LAUNCHER_LOOK_CREDIT: &str = "https://github.com/Xmilon";

/// The official launcher installer's product home.
pub const LAUNCHER_ABOUT: &str = "https://playsunrise.net";

/// Where the game repository publishes each game build's mod SDK (`sdk-<build id>`) and the game
/// releases whose `sdk.json` names their build; a release's tag and file follow.
/// `SUNRISE_SDK_BASE` overrides it.
pub const SDK_RELEASES: &str = "https://github.com/sunrise-org/sunrise/releases/download";
