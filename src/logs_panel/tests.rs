use super::*;

#[test]
fn a_crash_opens_the_panel_and_a_clean_exit_does_not() {
    // An exit status that isn't success, or a crash report.
    assert!(crash_opens_logs(true, false, false));
    assert!(crash_opens_logs(false, true, false));
    assert!(crash_opens_logs(true, true, false));
    // The game quit on its own, or the player pressed Stop.
    assert!(!crash_opens_logs(false, false, false));
    assert!(!crash_opens_logs(true, false, true));
    assert!(!crash_opens_logs(true, true, true));
}

#[test]
fn a_crash_shows_the_game_tab_from_its_newest_line() {
    let mut logs = LogsPanel {
        paused: true,
        drawn: Some((3, 3)),
        ..LogsPanel::default()
    };
    logs.show_game(Instant::now());
    assert!(logs.open);
    assert!(logs.fade.is_some_and(|fade| fade.on));
    assert!(logs.tab == Tab::Game);
    assert!(!logs.paused);
    assert_eq!(logs.drawn, None);
}

#[test]
fn a_crash_leaves_an_open_panel_open() {
    let now = Instant::now();
    let mut logs = LogsPanel::default();
    logs.set_open(true, now);
    logs.paused = true;
    logs.show_game(now);
    assert!(logs.open, "a second open must not close it again");
    assert!(logs.tab == Tab::Game);
    assert!(!logs.paused);
    assert!(logs.fade.is_some_and(|fade| fade.on));
}
