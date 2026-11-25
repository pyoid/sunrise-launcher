//! How long each stage of a native build took, for the one summary line a report quotes.
use std::time::Duration;

/// The stages of a build in the order they run. A stage that didn't run (a build that was
/// already up to date stops after the check) stays `None` and is left out of the summary.
#[derive(Default)]
pub(super) struct Stages {
    pub reading: Option<Duration>,
    pub planning: Option<Duration>,
    pub check: Option<Duration>,
    pub prebuilt: Option<Duration>,
    pub toolchain: Option<Duration>,
    pub package: Option<Duration>,
    pub staging: Option<Duration>,
    pub tool: Option<Duration>,
    pub verify: Option<Duration>,
    pub install: Option<Duration>,
}

impl Stages {
    /// `Stage times: reading 12 ms, planning 40 ms, ...`, build-log text like the lines it sums up.
    pub fn summary(&self) -> String {
        let ran: Vec<String> = [
            ("reading", self.reading),
            ("planning", self.planning),
            ("check", self.check),
            ("prebuilt", self.prebuilt),
            ("toolchain", self.toolchain),
            ("package", self.package),
            ("staging", self.staging),
            ("tool", self.tool),
            ("verify", self.verify),
            ("install", self.install),
        ]
        .into_iter()
        .filter_map(|(name, took)| Some(format!("{name} {} ms", took?.as_millis())))
        .collect();
        format!("Stage times: {}", ran.join(", "))
    }
}

#[cfg(test)]
mod tests;
