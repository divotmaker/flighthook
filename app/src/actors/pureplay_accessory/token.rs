//! The Pureplay Accessory card's pairing tokens: one file in the user's data folder,
//! never the config file, so config backups, exports and shared screenshots
//! never carry a token.
//!
//! The file (`pureplay-tokens.json`) maps a card's normalized address
//! ([`flighthook::address_key`]) to its pairing: `{ token, clientId,
//! paired_at }`. Changing a card's address therefore pairs it again. The
//! file is written atomically (a temporary file, then a rename) and is
//! readable by its owner only (0600 on Unix; on Windows the per-user data
//! folder). A file that is not a token file is read as empty, warned about
//! once, and copied to `pureplay-tokens.json.bad` before the next write
//! replaces it.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// The token file's name, in flighthook's data folder.
pub const FILE_NAME: &str = "pureplay-tokens.json";

/// What replaces a token wherever a frame is logged or shown.
pub const REDACTED: &str = "<redacted>";

/// The default token file: `flighthook/pureplay-tokens.json` in the user's
/// local data folder (`%LOCALAPPDATA%\flighthook`,
/// `~/.local/share/flighthook`).
#[must_use]
pub fn default_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flighthook")
        .join(FILE_NAME)
}

/// A pairing token. Opaque; its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token itself, for the `pair` message and the token file only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// `text` with every occurrence of each of `tokens`, as written and as
/// escaped inside a JSON string, replaced by [`REDACTED`]. The rest of the
/// text is unchanged, so a redacted frame is otherwise byte for byte the
/// frame on the wire.
#[must_use]
pub fn redact<'a>(text: &str, tokens: impl IntoIterator<Item = &'a Token>) -> String {
    let mut out = text.to_owned();
    for token in tokens {
        let raw = token.expose();
        if raw.is_empty() {
            continue;
        }
        out = out.replace(raw, REDACTED);
        // A token with characters JSON escapes appears escaped in a frame.
        if let Ok(quoted) = serde_json::to_string(raw) {
            let escaped = &quoted[1..quoted.len() - 1];
            if escaped != raw {
                out = out.replace(escaped, REDACTED);
            }
        }
    }
    out
}

/// One saved pairing. No `Debug`: it holds the token.
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    token: String,
    #[serde(rename = "clientId", default)]
    client_id: String,
    #[serde(default)]
    paired_at: String,
}

/// The token file, read and written whole under a lock.
#[derive(Debug)]
pub struct TokenStore {
    path: PathBuf,
    lock: Mutex<()>,
    /// A file that is not a token file was warned about.
    warned: AtomicBool,
}

/// What the file held when read.
enum Read {
    /// The saved pairings (none when there is no file).
    Entries(BTreeMap<String, Entry>),
    /// A file that is not a token file.
    Unreadable,
}

impl TokenStore {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
            warned: AtomicBool::new(false),
        }
    }

    /// The token file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The token saved for address key `key`, if any. A file that cannot
    /// be read counts as empty.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Token> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.entries()
            .remove(key)
            .map(|e| Token(e.token))
            .filter(|t| !t.0.is_empty())
    }

    /// Save `token` (with Pureplay's `client_id`) for address key `key`.
    pub fn save(&self, key: &str, token: &Token, client_id: &str) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.entries_to_replace()?;
        entries.insert(
            key.to_owned(),
            Entry {
                token: token.0.clone(),
                client_id: client_id.to_owned(),
                paired_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
        );
        self.write(&entries)
    }

    /// Delete the token saved for address key `key`. Returns whether there
    /// was one.
    pub fn forget(&self, key: &str) -> std::io::Result<bool> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.entries_to_replace()?;
        if entries.remove(key).is_none() {
            return Ok(false);
        }
        self.write(&entries)?;
        Ok(true)
    }

    fn read(&self) -> Read {
        match std::fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(entries) => Read::Entries(entries),
                Err(e) => {
                    if !self.warned.swap(true, Ordering::Relaxed) {
                        tracing::warn!(
                            "pureplay_accessory: {} is not a token file ({e}), read as empty",
                            self.path.display()
                        );
                    }
                    Read::Unreadable
                }
            },
            Err(_) => Read::Entries(BTreeMap::new()),
        }
    }

    /// The saved pairings; none when the file cannot be read.
    fn entries(&self) -> BTreeMap<String, Entry> {
        match self.read() {
            Read::Entries(entries) => entries,
            Read::Unreadable => BTreeMap::new(),
        }
    }

    /// The saved pairings, before a write replaces the file. A file that is
    /// not a token file is copied to `<file>.bad` first, so what it held is
    /// not lost.
    fn entries_to_replace(&self) -> std::io::Result<BTreeMap<String, Entry>> {
        match self.read() {
            Read::Entries(entries) => Ok(entries),
            Read::Unreadable => {
                let bad = self.bad_path();
                std::fs::copy(&self.path, &bad)?;
                tracing::warn!(
                    "pureplay_accessory: the unreadable token file was kept as {}",
                    bad.display()
                );
                Ok(BTreeMap::new())
            }
        }
    }

    /// Where an unreadable token file is kept: `<file>.bad`.
    fn bad_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".bad");
        self.path.with_file_name(name)
    }

    /// Write `entries` atomically: a temporary file readable by the owner
    /// only, then a rename over the file.
    fn write(&self, entries: &BTreeMap<String, Entry>) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(entries).map_err(std::io::Error::other)?;
        let name = self
            .path
            .file_name()
            .map_or_else(|| FILE_NAME.into(), |s| s.to_string_lossy());
        let tmp = self
            .path
            .with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
        let result = owner_only_file(&tmp)
            .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
            .and_then(|()| std::fs::rename(&tmp, &self.path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

/// A new file at `path` that only its owner can read and write.
fn owner_only_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token store in a fresh folder, which goes when the test is done.
    struct Store(TokenStore);

    impl std::ops::Deref for Store {
        type Target = TokenStore;
        fn deref(&self) -> &TokenStore {
            &self.0
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            if let Some(dir) = self.0.path().parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn store() -> Store {
        Store(TokenStore::new(std::env::temp_dir().join(format!(
            "flighthook-pp-tokens-{}/{FILE_NAME}",
            uuid::Uuid::new_v4().simple()
        ))))
    }

    #[test]
    fn a_token_is_saved_read_and_forgotten_by_address() {
        let store = store();
        assert_eq!(store.get("127.0.0.1:9770"), None);
        let token = Token::new("PPA-6F1C2A9B3D4E4F50A1B2C3D4E5F60718");
        store.save("127.0.0.1:9770", &token, "B1E7").expect("save");
        assert_eq!(store.get("127.0.0.1:9770"), Some(token.clone()));
        assert_eq!(store.get("192.168.1.20:9770"), None, "another address");
        let text = std::fs::read_to_string(store.path()).expect("file");
        let json: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["127.0.0.1:9770"]["token"], token.expose());
        assert_eq!(json["127.0.0.1:9770"]["clientId"], "B1E7");
        assert!(json["127.0.0.1:9770"]["paired_at"].is_string());
        assert!(store.forget("127.0.0.1:9770").expect("forget"));
        assert!(!store.forget("127.0.0.1:9770").expect("forget again"));
        assert_eq!(store.get("127.0.0.1:9770"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let store = store();
        store
            .save("127.0.0.1:9770", &Token::new("t"), "c")
            .expect("save");
        let mode = std::fs::metadata(store.path())
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir = store.path().parent().expect("dir");
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .expect("dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temporary file left");
    }

    /// A file that is not a token file reads as empty and is kept as
    /// `.bad` before a save replaces it.
    #[test]
    fn an_unreadable_file_is_kept_before_it_is_replaced() {
        let store = store();
        std::fs::create_dir_all(store.path().parent().expect("dir")).expect("dir");
        std::fs::write(store.path(), b"{ not json").expect("write");
        assert_eq!(store.get("127.0.0.1:9770"), None);
        assert_eq!(store.get("127.0.0.1:9770"), None, "asked again");
        store
            .save("127.0.0.1:9770", &Token::new("PPA-NEW"), "c")
            .expect("save");
        assert_eq!(store.get("127.0.0.1:9770"), Some(Token::new("PPA-NEW")));
        let bad = std::fs::read(store.bad_path()).expect("the .bad copy");
        assert_eq!(bad, b"{ not json");
    }

    #[test]
    fn a_token_never_shows_in_debug_and_is_redacted_in_text() {
        let token = Token::new("PPA-secret");
        assert_eq!(format!("{token:?}"), REDACTED);
        let frame = r#"{"type":"paired","data":{"token":"PPA-secret","clientId":"x"}}"#;
        assert_eq!(
            redact(frame, [&token]),
            r#"{"type":"paired","data":{"token":"<redacted>","clientId":"x"}}"#
        );
        let odd = Token::new(r#"a"b\c"#);
        let frame = serde_json::json!({"token": odd.expose()}).to_string();
        assert_eq!(redact(&frame, [&odd]), r#"{"token":"<redacted>"}"#);
    }
}
