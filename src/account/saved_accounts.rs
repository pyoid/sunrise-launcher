//! accounts.json: the signed-in accounts, the one that plays, and why none can.
use super::*;

/// accounts.json: the signed-in accounts and the one that plays. Never a token.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Accounts {
    pub accounts: Vec<Account>,
    /// The UUID of the account the game starts with; none: the first one.
    pub selected: Option<String>,
}

/// The secrets an account keeps in `store`, if it still has them there.
pub(super) fn secrets_in(store: &dyn SecretStore, uuid: &str) -> Option<Secrets> {
    store
        .get(uuid)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

impl Accounts {
    pub(crate) fn path() -> PathBuf {
        crate::backend::data_dir().join("accounts.json")
    }
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())
    }
    pub(crate) fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }
    pub(crate) fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // A new file renamed over the old one: a crash or a full disk mid-write leaves the old list,
        // not an empty file that reads as no accounts at all.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if let Err(e) = std::fs::write(&tmp, text + "\n").and_then(|()| std::fs::rename(&tmp, path))
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.to_string());
        }
        Ok(())
    }
    /// Saves what a launch renewed (names, skins) into `accounts.json` as it is now, under the
    /// accounts' lock: an account signed in or out while the services were asked stays so, where
    /// saving this copy, loaded before, would undo it.
    pub fn save_renewed(&self) -> Result<(), String> {
        self.save_renewed_to(store(), &Self::path())
    }
    pub(crate) fn save_renewed_to(
        &self,
        store: &dyn SecretStore,
        path: &Path,
    ) -> Result<(), String> {
        let _operation = session::lock(store)?;
        let mut saved = Self::load_from(path);
        for account in &self.accounts {
            saved.update(account.clone());
        }
        if saved.selected().is_none() {
            saved.selected = self
                .selected
                .clone()
                .filter(|u| saved.accounts.iter().any(|a| &a.uuid == u));
        }
        saved.save_to(path)
    }
    /// Records what a Save on the Skins tab left account `uuid` wearing, in `accounts.json` as it
    /// is now and under the accounts' lock: only the fields that Save changed (`skin`, `cape`;
    /// none leaves a field as it is), so nothing else about that account or any other is
    /// written back from an older copy.
    pub fn save_look(
        uuid: &str,
        skin: Option<Option<(String, String)>>,
        cape: Option<Option<String>>,
    ) -> Result<(), String> {
        Self::save_look_to(store(), &Self::path(), uuid, skin, cape)
    }
    pub(crate) fn save_look_to(
        store: &dyn SecretStore,
        path: &Path,
        uuid: &str,
        skin: Option<Option<(String, String)>>,
        cape: Option<Option<String>>,
    ) -> Result<(), String> {
        let _operation = session::lock(store)?;
        let mut saved = Self::load_from(path);
        // Signed out meanwhile: there is nothing left to record it on.
        let Some(account) = saved.accounts.iter_mut().find(|a| a.uuid == uuid) else {
            return Ok(());
        };
        if let Some(skin) = skin {
            account.skin = skin;
        }
        if let Some(cape) = cape {
            account.cape = cape;
        }
        saved.save_to(path)
    }
    pub fn selected(&self) -> Option<&Account> {
        let uuid = self.selected.as_ref()?;
        self.accounts.iter().find(|a| &a.uuid == uuid)
    }
    /// Find an account by name (any case) or UUID (with or without dashes).
    pub fn find(&self, who: &str) -> Option<&Account> {
        let undashed = who.replace('-', "").to_lowercase();
        self.accounts
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(who) || a.uuid == undashed)
    }
    /// Keep a new or renewed account, its secrets in the store, and select it.
    pub fn add(
        &mut self,
        store: &dyn SecretStore,
        account: Account,
        secrets: &Secrets,
    ) -> Result<(), String> {
        let _operation = session::lock(store)?;
        self.keep_sign_in(store, account, secrets)
    }

    pub(super) fn keep_sign_in(
        &mut self,
        store: &dyn SecretStore,
        account: Account,
        secrets: &Secrets,
    ) -> Result<(), String> {
        store.set(
            &account.uuid,
            &serde_json::to_string(secrets).map_err(|e| e.to_string())?,
        )?;
        self.selected = Some(account.uuid.clone());
        match self.accounts.iter_mut().find(|a| a.uuid == account.uuid) {
            Some(a) => *a = account,
            None => self.accounts.push(account),
        }
        Ok(())
    }
    /// Whether an account here owned the game at its last check (the keychain's answer, no
    /// request): what Play needs.
    pub fn has_owner(&self, store: &dyn SecretStore) -> bool {
        // The selected account first: it owning the game ends the search after one account's
        // item — every account's item read can ask for the keychain (macOS, #4912).
        self.selected()
            .into_iter()
            .chain(self.accounts.iter().filter(|a| Some(*a) != self.selected()))
            .any(|a| secrets_in(store, &a.uuid).is_some_and(|s| s.owns))
    }

    /// Plays as the signed-in account `uuid`.
    pub fn play_as(&mut self, uuid: &str) -> Result<(), String> {
        if !self.accounts.iter().any(|a| a.uuid == uuid) {
            return Err(crate::i18n::tr("That account isn't signed in.").into());
        }
        self.selected = Some(uuid.to_string());
        Ok(())
    }

    /// Switches who plays to `uuid`, on disk: the list as it is saved now with that account
    /// selected, saved again. The window keeps what this returns.
    pub fn switch(uuid: &str) -> Result<Self, String> {
        Self::switch_in(&Self::path(), uuid)
    }
    pub(crate) fn switch_in(path: &Path, uuid: &str) -> Result<Self, String> {
        let mut accounts = Self::load_from(path);
        accounts.play_as(uuid)?;
        accounts.save_to(path)?;
        Ok(accounts)
    }

    /// Moves `uuid` by `delta` (`-1` up, `+1` down) in the account list. Returns whether the order
    /// changed.
    pub fn move_account(&mut self, uuid: &str, delta: isize) -> bool {
        let Some(from) = self.accounts.iter().position(|a| a.uuid == uuid) else {
            return false;
        };
        let Some(to) = from
            .checked_add_signed(delta)
            .filter(|&i| i < self.accounts.len())
        else {
            return false;
        };
        if from == to {
            return false;
        }
        self.accounts.swap(from, to);
        true
    }

    /// Reorders `uuid` by `delta` on disk and returns the saved list.
    pub fn reorder(uuid: &str, delta: isize) -> Result<Self, String> {
        Self::reorder_in(&Self::path(), uuid, delta)
    }
    pub(crate) fn reorder_in(path: &Path, uuid: &str, delta: isize) -> Result<Self, String> {
        let mut accounts = Self::load_from(path);
        if accounts.move_account(uuid, delta) {
            accounts.save_to(path)?;
        }
        Ok(accounts)
    }

    /// A renewed profile (its name or skin changed) in place of the old one; who plays stays.
    pub(super) fn update(&mut self, account: Account) {
        if let Some(a) = self.accounts.iter_mut().find(|a| a.uuid == account.uuid) {
            *a = account;
        }
    }

    /// Sign out: the secrets leave the keychain, the account the list.
    pub fn remove(&mut self, store: &dyn SecretStore, uuid: &str) -> Result<(), String> {
        let _operation = session::lock(store)?;
        store.delete(uuid)?;
        self.accounts.retain(|a| a.uuid != uuid);
        if self.selected.as_deref() == Some(uuid) {
            self.selected = None;
        }
        Ok(())
    }
}

/// No account in the launcher owns Minecraft: Java Edition: the game must not start.
#[derive(Debug, Default, PartialEq)]
pub struct NoOwner {
    /// Why a sign-in couldn't be read, when the keyring didn't give it (#12179), or why the
    /// account that plays can't: what is said instead of the refusal, which would tell a
    /// signed-in player to sign in.
    pub keyring: Option<String>,
}
impl std::fmt::Display for NoOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.keyring {
            Some(why) => write!(f, "{why}"),
            None => write!(f, "{}.", crate::i18n::tr(NEEDS_OWNER)),
        }
    }
}

impl NoOwner {
    /// The same refusal for the window, which has Settings rather than a command to name.
    pub fn in_window(&self) -> String {
        use crate::i18n::tr;
        if let Some(why) = &self.keyring {
            return why.clone();
        }
        format!(
            "{}. {}",
            tr(NEEDS_OWNER),
            tr("Open Settings and sign in under Account.")
        )
    }
}
