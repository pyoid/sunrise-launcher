use super::*;

#[test]
fn a_sign_in_trace_carries_no_secret_and_keeps_hosts_and_statuses() {
    let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlMTIzNDU2";
    let trace = [
        "Signing in: login.microsoftonline.com".to_owned(),
        "POST login.microsoftonline.com 200".to_owned(),
        "POST user.auth.xboxlive.com 200".to_owned(),
        "POST xsts.auth.xboxlive.com 401".to_owned(),
        "POST api.minecraftservices.com 200".to_owned(),
        format!("response token {jwt} accepted"),
        "redirect https://login.microsoftonline.com/cb?access_token=abc123def&state=1".to_owned(),
        r#"{"user_code":"ZXCV9876","device_code":"dev1","expires_in":900}"#.to_owned(),
        r#"{"code":"M.C507_BAY.2.U.qwerty"}"#.to_owned(),
        "Authorization: Bearer shorttok".to_owned(),
        "refresh_token=r.1.2&client_secret=hush&password=pw1".to_owned(),
        "account 069a79f4-44e9-4726-a5be-fca90e38aaf5 is Steve_Tester".to_owned(),
    ]
    .join("\n");
    let names = Scrubber::new().term("Steve_Tester");
    let out = hide_secrets(&names.scrub(&trace));
    for secret in [
        "eyJ",
        "abc123def",
        "ZXCV9876",
        "M.C507_BAY",
        "qwerty",
        "shorttok",
        "r.1.2",
        "hush",
        "pw1",
        "069a79f4",
        "Steve_Tester",
    ] {
        assert!(!out.contains(secret), "{secret} survived in:\n{out}");
    }
    for kept in [
        "login.microsoftonline.com 200",
        "user.auth.xboxlive.com 200",
        "xsts.auth.xboxlive.com 401",
        "api.minecraftservices.com 200",
        "state=1",
        "expires_in",
    ] {
        assert!(out.contains(kept), "{kept} lost in:\n{out}");
    }
}

#[test]
fn a_sha256_and_a_sentence_pass_unchanged() {
    let sha = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    let sentence = "Checking game files: 1204 of 5310 verified, nothing to download";
    assert_eq!(scrub(sha), sha);
    assert_eq!(scrub(sentence), sentence);
}

#[test]
fn since_returns_only_newer_lines_and_the_ring_keeps_the_newest() {
    let mut ring = Ring::new();
    for n in 0..3 {
        ring.push(format!("line {n}"));
    }
    assert_eq!(
        ring.since(0),
        (3, vec!["line 0".into(), "line 1".into(), "line 2".into()])
    );
    assert_eq!(ring.since(2), (3, vec!["line 2".into()]));
    assert_eq!(ring.since(3), (3, Vec::new()));
    for n in 3..RING_LINES + 10 {
        ring.push(format!("line {n}"));
    }
    let (revision, lines) = ring.since(0);
    assert_eq!(revision, RING_LINES as u64 + 10);
    assert_eq!(lines.len(), RING_LINES);
    assert_eq!(lines[0], "line 10");
    assert_eq!(lines[RING_LINES - 1], format!("line {}", RING_LINES + 9));
    let (_, newer) = ring.since(revision - 2);
    assert_eq!(newer.len(), 2);
}

#[test]
fn the_file_rotates_and_keeps_three_old_files() {
    let dir = tempfile::tempdir().expect("a scratch folder");
    let mut file = LogFile::open(dir.path(), 100);
    for n in 0..60 {
        file.write(&format!("line number {n:03} of the rotation test"));
    }
    let names: Vec<String> = fs::read_dir(dir.path())
        .expect("the logs folder")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for kept in [
        "launcher.log",
        "launcher.1.log",
        "launcher.2.log",
        "launcher.3.log",
    ] {
        assert!(
            names.iter().any(|n| n == kept),
            "{kept} missing in {names:?}"
        );
    }
    assert_eq!(names.len(), 4, "{names:?}");
    let newest = fs::read_to_string(dir.path().join("launcher.log")).expect("the new file");
    assert!(newest.contains("line number 059"));
    assert!(newest.len() as u64 <= 100 + 41);
}

#[test]
fn a_step_dropped_without_a_result_logs_that_it_ended() {
    let before = since(0).0;
    drop(step("drop-test step alpha"));
    step("drop-test step beta").done("");
    step("drop-test step gamma").failed("no reason");
    let (_, lines) = since(before);
    let has = |text: &str| lines.iter().any(|l| l.contains(text));
    assert!(has("drop-test step alpha: ended after "));
    assert!(has("drop-test step beta: done in "));
    assert!(has("drop-test step gamma: failed after "));
    assert!(!has("drop-test step beta: ended"));
    assert!(!has("drop-test step gamma: ended"));
}

#[test]
fn a_term_registered_after_the_write_is_scrubbed_on_read() {
    const WORLD: &str = "Fixture world discovered after the write";
    let before = since(0).0;
    line(format!("Late world test: {WORLD}"));
    hide(WORLD);
    let (_, lines) = since(before);
    let saved = lines
        .iter()
        .find(|line| line.contains("Late world test:"))
        .unwrap();
    assert!(saved.contains(WORLD));
    assert!(scrub(saved).contains("Late world test: <private>"));
}

#[test]
fn a_preparation_line_is_logged_without_the_players_name() {
    hide("Steve_Prepare");
    let before = since(0).0;
    prepare_line("12 of 4000 files for Steve_Prepare");
    prepare_line("Session ID is legacy-preparation-credential");
    let (_, lines) = since(before);
    let found: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("Preparing: 12 of 4000 files"))
        .collect();
    assert_eq!(found.len(), 1, "{lines:?}");
    assert!(!found[0].contains("Steve_Prepare"), "{found:?}");
    assert!(!lines
        .iter()
        .any(|line| line.contains("legacy-preparation-credential")));
}

#[test]
fn play_meter_reports_counted_steps_to_both_ui_and_log() {
    let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let sink_seen = seen.clone();
    let before = since(0).0;
    let _guard = on_play_detail(move |text| {
        lock(&sink_seen).push(text);
    });
    let meter = PlayMeter::new(|done, total| format!("Sealing test files: {done} of {total}"));
    meter.tick(1, 100);
    meter.tick(50, 100);
    meter.tick(100, 100);
    let ui: Vec<String> = lock(&seen)
        .iter()
        .filter(|l| l.starts_with("Sealing test files:"))
        .cloned()
        .collect();
    assert_eq!(
        ui,
        vec![
            "Sealing test files: 1 of 100".to_string(),
            "Sealing test files: 100 of 100".to_string(),
        ]
    );
    let (_, lines) = since(before);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Sealing test files: 1 of 100")),
        "{lines:?}"
    );
}

#[test]
fn play_detail_from_spawned_thread_reaches_sink() {
    let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let sink_seen = seen.clone();
    let _guard = on_play_detail(move |text| {
        lock(&sink_seen).push(text);
    });
    play_detail_ui_only("spawned-thread-test: Downloading Minecraft's files: 12 of 400");
    let meter = PlayMeter::new(|done, total| {
        format!("spawned-thread-test: Sealing worker files: {done} of {total}")
    });
    std::thread::spawn(move || {
        meter.tick(1, 10);
        meter.tick(10, 10);
        meter.tick(6, 10); // A completed worker can report late.
    })
    .join()
    .expect("worker thread joined");
    let got: Vec<String> = lock(&seen)
        .iter()
        .filter(|l| l.starts_with("spawned-thread-test:"))
        .cloned()
        .collect();
    assert_eq!(
        got,
        vec![
            "spawned-thread-test: Downloading Minecraft's files: 12 of 400".to_string(),
            "spawned-thread-test: Sealing worker files: 1 of 10".to_string(),
            "spawned-thread-test: Sealing worker files: 10 of 10".to_string(),
        ]
    );
}
