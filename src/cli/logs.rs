//! `logs`, `share-log`, `unshare-log` and `bug-report`: the game's log, shared privately or not.
use super::*;

pub(super) fn show(lines: usize, crash: bool, as_json: bool) -> Result<(), Error> {
    let (path, text) = backend::read_logs(lines, crash)?;
    if as_json {
        json(serde_json::json!({ "path": path, "text": text }));
    } else {
        print!("{text}");
    }
    Ok(())
}

pub(super) fn share(instance: Option<String>, as_json: bool) -> Result<(), Error> {
    use sunrise_launcher::game_log;
    let i = match instance {
        Some(name) => instances::find(&name)?,
        None => instances::selection()?,
    };
    let path = i.home.join("logs/latest.log");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let accounts = sunrise_launcher::account::Accounts::load().accounts;
    let names = game_log::names(
        &[&i.home, &backend::game_home()],
        accounts.into_iter().map(|a| a.name),
    );
    let text = game_log::private(&text, dirs::home_dir().as_deref(), &names);
    let shared = game_log::share(&game_log::api(), &text)?;
    if as_json {
        json(serde_json::json!({
            "url": shared.url, "delete": format!("{}:{}", shared.id, shared.token),
        }));
    } else {
        println!("{}", shared.url);
        println!(
            "Delete it: sunrise-launcher unshare-log {}:{}",
            shared.id, shared.token
        );
    }
    Ok(())
}

pub(super) fn unshare(shared: String) -> Result<(), Error> {
    let (id, token) = shared
        .split_once(':')
        .ok_or("Give the \"<id>:<token>\" share-log printed.")?;
    let api = sunrise_launcher::game_log::api();
    sunrise_launcher::game_log::unshare(
        &api,
        &sunrise_launcher::game_log::Shared {
            url: String::new(),
            id: id.into(),
            token: token.into(),
        },
    )?;
    println!("Deleted the shared log {id}.");
    Ok(())
}

pub(super) fn bug_report(text: String, log: bool, as_json: bool) -> Result<(), Error> {
    let path = sunrise_launcher::bug_report::write(&text, log)?;
    if as_json {
        json(serde_json::json!({ "path": path }));
    } else {
        println!(
            "Wrote {}: read it, then attach it to your issue.",
            path.display()
        );
    }
    Ok(())
}
