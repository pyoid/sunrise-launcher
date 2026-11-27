//! Signing in with Microsoft and refreshing: the whole chain, targeted sign-ins, retries, addresses and the steps it reports.

use super::*;

#[test]
fn the_whole_chain() {
    let (m, ep) = mock(|m| m.pending = 2);
    let code = device_code(&ep, "test-client").unwrap();
    assert_eq!(
        (code.user_code.as_str(), code.verification_uri.as_str()),
        ("ABCD-EFGH", "https://www.microsoft.com/link")
    );
    let ms = wait_for_sign_in(&ep, "test-client", &code, &|| false).unwrap();
    let (account, secrets) = finish_sign_in(&ep, ms).unwrap();
    assert_eq!(
        account,
        Account {
            uuid: UUID.into(),
            name: "Notch".into(),
            skin: Some((
                "http://textures.minecraft.net/texture/abc".into(),
                "slim".into()
            )),
            cape: Some("http://textures.minecraft.net/texture/c2".into()),
        }
    );
    assert_eq!(secrets.minecraft_token, "mc-token");
    assert_eq!(secrets.xuid.as_deref(), Some("2535400000000000"));
    assert!(secrets.minecraft_expires_at > now() + 80000);
    // Two pending polls, then the token; then Xbox, XSTS, Minecraft, entitlements, profile.
    let paths: Vec<String> = m
        .lock()
        .unwrap()
        .log
        .iter()
        .map(|(_, p, _)| p.clone())
        .collect();
    assert_eq!(
        paths,
        [
            "/ms/devicecode",
            "/ms/token",
            "/ms/token",
            "/ms/token",
            "/xbl",
            "/xsts",
            "/mc/launcher/login",
            "/mc/entitlements/mcstore",
            "/mc/minecraft/profile"
        ]
    );
    assert!(!format!("{secrets:?}").contains("mc-token"));
}

#[test]
fn login_with_xbox_signs_in_when_launcher_login_refuses() {
    for status in [401, 404, 503] {
        let (m, ep) = mock(|m| m.launcher_login_refused = Some(status));
        let ms = MsTokens {
            access: "ms-access-1".into(),
            refresh: "r".into(),
        };
        let (account, secrets) = finish_sign_in(&ep, ms).unwrap();
        assert_eq!(account.uuid, UUID);
        assert_eq!(secrets.minecraft_token, "mc-token");
        // The same owner check follows, with the token login_with_xbox gave.
        assert!(secrets.owns);
        let paths: Vec<String> = m
            .lock()
            .unwrap()
            .log
            .iter()
            .map(|(_, p, _)| p.clone())
            .collect();
        assert_eq!(
            paths[2..],
            [
                "/mc/launcher/login",
                "/mc/authentication/login_with_xbox",
                "/mc/entitlements/mcstore",
                "/mc/minecraft/profile"
            ]
        );
    }
    // Either way, an account that doesn't own the game is refused.
    let (_, ep) = mock(|m| {
        m.launcher_login_refused = Some(503);
        m.owns_game = false;
    });
    let ms = MsTokens {
        access: "ms-access-1".into(),
        refresh: "r".into(),
    };
    assert_eq!(
        finish_sign_in(&ep, ms).unwrap_err().to_string(),
        DOES_NOT_OWN
    );
}

#[test]
fn refresh_only_when_the_token_runs_out() {
    let (m, ep) = mock(|_| {});
    let Check::Owns(_, kept) = check(
        &ep,
        Some("test-client"),
        &notch(),
        secrets("mc-token", true),
    ) else {
        panic!("owns");
    };
    assert_eq!(kept.refresh_token, "ms-refresh-1");
    // Entitlements and profile only: the token had two hours left.
    assert_eq!(m.lock().unwrap().log.len(), 2);
    let stale = Secrets {
        minecraft_expires_at: now() + 60,
        ..kept.clone()
    };
    let Check::Owns(renewed_account, renewed) = check(&ep, Some("test-client"), &notch(), stale)
    else {
        panic!("owns");
    };
    assert_eq!(
        (
            renewed.refresh_token.as_str(),
            renewed.minecraft_token.as_str()
        ),
        ("ms-refresh-2", "mc-token")
    );
    assert!(renewed_account.skin.is_some());
    let revoked = Secrets {
        refresh_token: "revoked".into(),
        minecraft_expires_at: 0,
        ..kept
    };
    assert!(matches!(
        check(&ep, Some("test-client"), &notch(), revoked),
        Check::NoAnswer(e, s) if e == SignInError::SignInAgain && s.owns
    ));
}

#[test]
fn a_refresh_token_of_another_app_registration_asks_to_sign_in_again() {
    let (_, ep) = mock(|_| {});
    let other = Secrets {
        refresh_token: "other-app".into(),
        minecraft_expires_at: 0,
        ..secrets("mc-token", true)
    };
    assert!(matches!(
        check(&ep, Some("test-client"), &notch(), other),
        Check::NoAnswer(e, s) if e == SignInError::SignInAgain && s.sign_in_required
    ));
}

/// Each case reads as the very text it was a String before: the window translates sign-in errors
/// by that text, and the CLI and MCP print it.
#[test]
fn sign_in_errors_read_as_before() {
    for (error, before) in [
        (SignInError::SignInAgain, "Your Microsoft sign-in expired. Sign in again."),
        (SignInError::DamagedSignIn, "Could not read the saved sign-in. Sign in again."),
        (
            SignInError::NotConfigured,
            "This build of the launcher has no Microsoft sign-in (SUNRISE_MSA_CLIENT_ID), so it can't start the game.",
        ),
        (SignInError::DoesNotOwn, "This account doesn't own Minecraft: Java Edition."),
        (
            SignInError::NoProfile,
            "This account has no Minecraft profile yet. Create one at minecraft.net, then sign in again.",
        ),
        (
            SignInError::OtherProfile,
            "The Microsoft account now has another Minecraft profile. Sign in again.",
        ),
        (SignInError::Other("Sign-in cancelled.".into()), "Sign-in cancelled."),
    ] {
        assert_eq!(error.to_string(), before);
        assert_eq!(String::from(error), before);
    }
    // The cases the code acts on come from the answers that meant them.
    assert_eq!(
        oauth_error(&json!({"error": "invalid_grant"})),
        SignInError::SignInAgain
    );
    assert_eq!(
        oauth_error(&json!({"error": "expired_token"})),
        SignInError::Other("The sign-in code expired. Start again.".into())
    );
}

#[test]
fn cancelling_stops_the_wait() {
    let (_, ep) = mock(|m| m.pending = 100);
    let code = device_code(&ep, "test-client").unwrap();
    assert_eq!(
        wait_for_sign_in(&ep, "test-client", &code, &|| true)
            .unwrap_err()
            .to_string(),
        "Sign-in cancelled."
    );
}

#[test]
fn client_id_is_configuration() {
    // The run-time variable overrides; empty means not configured.
    assert_eq!(client_id_from(Some(" ".into())), None);
    assert_eq!(
        client_id_from(Some("00000000-1111-2222-3333-444444444444".into())).as_deref(),
        Some("00000000-1111-2222-3333-444444444444")
    );
}

#[test]
fn development_endpoint_overrides_win_over_the_built_in_hosts() {
    let ep = Endpoints::at(
        Some("http://127.0.0.1:9/msa"),
        Some("http://127.0.0.1:9/minecraft"),
    );
    assert_eq!(ep.microsoft, "http://127.0.0.1:9/msa");
    assert_eq!(ep.minecraft, "http://127.0.0.1:9/minecraft");
    // Without an override the built-in hosts are used, on release builds too.
    let ep = Endpoints::at(None, None);
    assert_eq!(ep.microsoft, sunrise_launcher_addresses::MICROSOFT_LOGIN);
    assert_eq!(ep.minecraft, sunrise_launcher_addresses::MINECRAFT_SERVICES);
    assert_eq!(ep.xbox_user, sunrise_launcher_addresses::XBOX_USER_AUTH);
    assert_eq!(ep.xsts, sunrise_launcher_addresses::XSTS_AUTH);
}

#[test]
fn manual_refresh_renews_a_fresh_token_without_selecting_another_account() {
    let store = MemoryStore::default();
    let accounts = signed_in(&store, &secrets("original-minecraft", true));
    let (m, ep) = mock(|_| {});
    let status = session::refresh_with(&ep, Some("test-client"), &store, &notch(), &nowhere());
    assert_eq!(status.state, session::State::Ready);
    let saved = kept(&store);
    assert_eq!(saved.refresh_token, "ms-refresh-2");
    assert_eq!(saved.minecraft_token, "mc-token");
    assert_eq!(accounts.selected.as_deref(), Some(UUID));
    assert_eq!(
        m.lock()
            .unwrap()
            .log
            .iter()
            .filter(|(_, path, _)| path == "/ms/token")
            .count(),
        1
    );
    let public = serde_json::to_string(&status).unwrap();
    for secret in ["ms-refresh-2", "mc-token", "original-minecraft"] {
        assert!(!public.contains(secret));
    }
}

#[test]
fn a_refresh_saves_a_renewed_profile_and_leaves_an_unchanged_one() {
    let store = MemoryStore::default();
    let accounts = signed_in(&store, &secrets("original-minecraft", true));
    let (_, ep) = mock(|_| {});
    let dir = std::env::temp_dir().join(format!("sunrise-launcher-refresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("accounts.json");
    accounts.save_to(&file).unwrap();
    // The services' profile has a skin and a cape now; the saved one has neither.
    let status = session::refresh_with(&ep, Some("test-client"), &store, &notch(), &file);
    assert_eq!(status.state, session::State::Ready);
    let saved = Accounts::load_from(&file);
    let account = saved.find(UUID).unwrap();
    assert!(account.skin.is_some() && account.cape.is_some());
    assert_eq!(saved.selected.as_deref(), Some(UUID));
    // Already the services' profile: the file is not written again.
    let text = serde_json::to_string(&saved).unwrap();
    std::fs::write(&file, &text).unwrap();
    let renewed = saved.find(UUID).unwrap().clone();
    session::refresh_with(&ep, Some("test-client"), &store, &renewed, &file);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revoked_refresh_is_remembered_until_a_successful_sign_in() {
    let store = MemoryStore::default();
    let mut accounts = signed_in(
        &store,
        &Secrets {
            refresh_token: "revoked".into(),
            ..secrets("old-minecraft", true)
        },
    );
    let (m, ep) = mock(|_| {});
    let status = session::refresh_with(&ep, Some("test-client"), &store, &notch(), &nowhere());
    assert_eq!(status.state, session::State::SignInRequired);
    let requests = m.lock().unwrap().log.len();
    let _ = session::refresh_with(&ep, Some("test-client"), &store, &notch(), &nowhere());
    let pass = launch(&ep, Some("test-client"), &store, &mut accounts).unwrap();
    assert!(pass.session.is_none());
    assert_eq!(pass.offline.as_deref(), Some(SIGN_IN_AGAIN));
    assert_eq!(m.lock().unwrap().log.len(), requests);
    assert_eq!(
        session::status(&store, UUID).state,
        session::State::SignInRequired
    );
    accounts
        .add(&store, notch(), &secrets("new-minecraft", true))
        .unwrap();
    assert_eq!(session::status(&store, UUID).state, session::State::Ready);
}

#[test]
fn a_connection_failure_is_retryable_and_keeps_the_saved_sign_in() {
    let store = MemoryStore::default();
    let _accounts = signed_in(&store, &secrets("still-usable", true));
    let before = store.get(UUID).unwrap();
    let status = session::refresh_with(
        &offline(),
        Some("test-client"),
        &store,
        &notch(),
        &nowhere(),
    );
    assert_eq!(status.state, session::State::Retry);
    assert_eq!(store.get(UUID).unwrap(), before);
    assert!(!kept(&store).sign_in_required);
}

#[test]
fn addresses_alternate_families_starting_with_the_first() {
    let v6 = |n: u16| SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], n));
    let v4 = |n: u16| SocketAddr::from(([127, 0, 0, 1], n));
    assert_eq!(
        interleave(vec![v6(1), v6(2), v6(3), v4(4), v4(5)]),
        vec![v6(1), v4(4), v6(2), v4(5), v6(3)]
    );
    assert_eq!(interleave(vec![v4(1), v6(2)]), vec![v4(1), v6(2)]);
    assert_eq!(
        interleave(vec![v4(1), v4(2), v4(3), v6(4), v6(5)]),
        vec![v4(1), v6(4), v4(2), v6(5), v4(3)]
    );
    assert_eq!(interleave(vec![v4(1), v4(2)]), vec![v4(1), v4(2)]);
    assert_eq!(interleave(vec![v6(1), v6(2)]), vec![v6(1), v6(2)]);
    assert!(interleave(Vec::new()).is_empty());
}

#[test]
fn a_dead_ipv6_address_falls_back_to_ipv4() {
    // Loopback only: a closed IPv6 port stands for the broken route, a local listener for the host's
    // IPv4 address. Both IPv6 answers come first, as a resolver lists them.
    let dead = |p: u16| SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], p));
    let closed = TcpListener::bind("[::1]:0")
        .map(|l| l.local_addr().unwrap().port())
        .unwrap_or(9);
    let live = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = live.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut conn, _) = live.accept().unwrap();
        let mut line = String::new();
        BufReader::new(conn.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        conn.write_all(
            b"HTTP/1.1 200 OK
Content-Length: 2
Connection: close

ok",
        )
        .unwrap();
    });
    let addrs = vec![
        dead(closed),
        dead(closed),
        SocketAddr::from(([127, 0, 0, 1], port)),
    ];
    let agent = sign_in_agent(ureq::AgentBuilder::new())
        .resolver(Interleaved(Fixed(addrs)))
        .build();
    let body = agent
        .get("http://sunrise.test/")
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert_eq!(body, "ok");
    server.join().unwrap();
}

#[test]
fn a_rotated_microsoft_credential_survives_a_later_service_failure() {
    let store = MemoryStore::default();
    let _accounts = signed_in(&store, &secrets("old-minecraft", true));
    let (_, ep) = mock(|m| m.xerr = Some(2148916238));
    let status = session::refresh_with(&ep, Some("test-client"), &store, &notch(), &nowhere());
    assert_eq!(status.state, session::State::Retry);
    assert_eq!(kept(&store).refresh_token, "ms-refresh-2");
    assert_eq!(kept(&store).minecraft_token, "old-minecraft");
}

#[test]
fn expiry_is_refreshable_and_a_missing_credential_needs_sign_in() {
    let store = MemoryStore::default();
    assert_eq!(
        session::status(&store, UUID).state,
        session::State::SignInRequired
    );
    let mut accounts = signed_in(
        &store,
        &Secrets {
            minecraft_expires_at: 0,
            ..secrets("expired", true)
        },
    );
    assert_eq!(
        session::status(&store, UUID).state,
        session::State::NeedsRefresh
    );
    let (_, ep) = mock(|_| {});
    assert!(launch(&ep, Some("test-client"), &store, &mut accounts)
        .unwrap()
        .session
        .is_some());
    assert_eq!(session::status(&store, UUID).state, session::State::Ready);
}

#[test]
fn refresh_serializes_with_sign_out_and_cannot_restore_a_deleted_credential() {
    let store = Arc::new(MemoryStore::default());
    let mut accounts = signed_in(&store, &secrets("old-minecraft", true));
    let (_, ep) = mock(|_| {});
    let refresh_store = store.clone();
    let refresh = std::thread::spawn(move || {
        session::refresh_with(
            &ep,
            Some("test-client"),
            &*refresh_store,
            &notch(),
            &nowhere(),
        )
    });
    accounts.remove(&*store, UUID).unwrap();
    let _ = refresh.join().unwrap();
    assert!(store.get(UUID).unwrap().is_none());
}

#[test]
fn targeted_sign_in_rejects_another_identity_without_writing_or_selecting_it() {
    let store = MemoryStore::default();
    let mut accounts = signed_in(&store, &secrets("original", true));
    let before = store.get(UUID).unwrap();
    let mut wrong = notch();
    wrong.uuid = "11111111111111111111111111111111".into();
    wrong.name = "OtherPlayer".into();
    let err = accept_sign_in(
        &mut accounts,
        &store,
        Some(&notch()),
        wrong.clone(),
        &secrets("wrong", true),
        &|| false,
        &|_| {},
    )
    .unwrap_err();
    assert!(err.contains("OtherPlayer") && err.contains("Notch"));
    assert_eq!(store.get(UUID).unwrap(), before);
    assert!(store.get(&wrong.uuid).unwrap().is_none());
    assert_eq!(accounts.accounts, vec![notch()]);
    assert_eq!(accounts.selected.as_deref(), Some(UUID));
}

#[test]
fn targeted_sign_in_preserves_the_selected_account() {
    let store = MemoryStore::default();
    let mut accounts = signed_in(&store, &secrets("old", true));
    let mut other = notch();
    other.uuid = "11111111111111111111111111111111".into();
    accounts
        .add(&store, other.clone(), &secrets("other", true))
        .unwrap();
    accept_sign_in(
        &mut accounts,
        &store,
        Some(&notch()),
        notch(),
        &secrets("renewed", true),
        &|| false,
        &|_| {},
    )
    .unwrap();
    assert_eq!(accounts.selected.as_deref(), Some(other.uuid.as_str()));
    assert_eq!(kept(&store).minecraft_token, "renewed");
}

#[test]
fn cancelled_or_removed_targeted_sign_in_keeps_credentials_unchanged() {
    let store = MemoryStore::default();
    let mut accounts = signed_in(&store, &secrets("original", true));
    let before = store.get(UUID).unwrap();
    assert!(accept_sign_in(
        &mut accounts,
        &store,
        Some(&notch()),
        notch(),
        &secrets("new", true),
        &|| true,
        &|_| {}
    )
    .is_err());
    assert_eq!(store.get(UUID).unwrap(), before);
    accounts.accounts.clear();
    assert!(accept_sign_in(
        &mut accounts,
        &store,
        Some(&notch()),
        notch(),
        &secrets("new", true),
        &|| false,
        &|_| {}
    )
    .is_err());
    assert_eq!(store.get(UUID).unwrap(), before);
}

#[test]
fn refresh_reports_real_service_steps_in_order() {
    use session::Phase::*;
    let store = MemoryStore::default();
    let _accounts = signed_in(&store, &secrets("old", true));
    let (_, ep) = mock(|_| {});
    let phases = std::cell::RefCell::new(Vec::new());
    let status = session::refresh_account(
        &ep,
        Some("test-client"),
        &store,
        &notch(),
        &nowhere(),
        &|phase| phases.borrow_mut().push(phase),
    );
    assert_eq!(status.state, session::State::Ready);
    assert_eq!(
        *phases.borrow(),
        [Queued, Keychain, Microsoft, Xbox, Minecraft, Ownership, Profile, Saving]
    );
    let mut accounts = signed_in(&store, &secrets("old", true));
    let (account, secrets) = finish_sign_in_with_progress(
        &ep,
        MsTokens {
            access: "ms-access-1".into(),
            refresh: "fresh".into(),
        },
        &|phase| phases.borrow_mut().push(phase),
    )
    .unwrap();
    accept_sign_in(
        &mut accounts,
        &store,
        Some(&notch()),
        account,
        &secrets,
        &|| false,
        &|phase| phases.borrow_mut().push(phase),
    )
    .unwrap();
    assert_eq!(
        &phases.borrow()[8..],
        &[Xbox, Minecraft, Ownership, Profile, Saving]
    );
}

#[test]
fn a_status_read_gives_up_behind_a_long_operation() {
    let store = MemoryStore::default();
    let held = session::lock(&store).unwrap();
    let start = std::time::Instant::now();
    let busy = session::lock_within(&store, Some(std::time::Duration::from_millis(150)));
    assert!(busy.is_err(), "a held lock must not be taken twice");
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    // Another memory store (another test running beside this one) has its own lock.
    let other = MemoryStore::default();
    assert!(session::lock_within(&other, Some(std::time::Duration::ZERO)).is_ok());
    drop(held);
    assert!(session::lock_within(&store, Some(std::time::Duration::from_secs(5))).is_ok());
}

/// A whole sign-in leaves each stage and each request's service, host and status in the launcher
/// log, and none of the tokens, the account's ids or its name.
#[test]
fn a_sign_in_logs_its_stages_and_requests_and_no_secret() {
    let (_, ep) = mock(|_| {});
    let before = crate::game_log::launcher::since(0).0;
    let ms = MsTokens {
        access: "ms-access-1".into(),
        refresh: "ms-refresh-1".into(),
    };
    finish_sign_in(&ep, ms).unwrap();
    // Other tests log beside this one: keep the sign-in's own lines.
    let (_, lines) = crate::game_log::launcher::since(before);
    let log: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("Sign-in: ") || l.contains(" -> "))
        .collect();
    let has = |text: &str| log.iter().any(|l| l.contains(text));
    for kept in [
        "Sign-in: Xbox Live user authentication: done in ",
        "Sign-in: XSTS authorization: done in ",
        "Sign-in: Minecraft login: done in ",
        "Sign-in: reading the Minecraft profile: done in ",
        "Xbox Live: POST ",
        " -> 200 (",
    ] {
        assert!(has(kept), "{kept:?} missing in {log:#?}");
    }
    for secret in [
        "ms-access-1",
        "ms-refresh-1",
        "xbl-token",
        "xsts-token",
        "uhs-1",
        "2535400000000000",
        "mc-token",
        UUID,
        "Notch",
    ] {
        assert!(!has(secret), "{secret:?} in {log:#?}");
    }
}

#[test]
fn device_code_poll_is_logged_as_waiting_without_its_code() {
    let (_, ep) = mock(|m| m.pending = 1);
    let code = device_code(&ep, "test-client").unwrap();
    let before = crate::game_log::launcher::since(0).0;
    assert!(matches!(
        poll(&ep, "test-client", &code).unwrap(),
        Poll::Pending
    ));
    let (_, lines) = crate::game_log::launcher::since(before);
    assert!(lines
        .iter()
        .any(|line| line.contains("Microsoft: POST ") && line.contains(" -> waiting for sign-in")));
    assert!(!lines
        .iter()
        .any(|line| line.contains("dev-1") || line.contains("ABCD-EFGH")));
}
