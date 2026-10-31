//! What Patina keeps about Jellyfin between runs: `jellyfin.json` in the
//! data folder, with the servers added, the account signed in on each and
//! this installation's device id, and each account's token in the
//! system's store (see `credentials`), never in the file.
//!
//! The token functions call the system's store, which can block and may
//! ask to be unlocked: they belong on a worker thread, as every other
//! credentials call does. Without a store they fail, and the token then
//! lives only in memory, as long as Patina runs.

use super::auth::Identity;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use url::Url;

/// What tokens are saved under in the system's store; the account is
/// `<server id>/<user id>`.
pub const TOKEN_SERVICE: &str = "Patina Jellyfin";

/// The device's name on every server. It must not change: the server
/// rewrites what it stored for the device when it does.
pub const DEVICE_NAME: &str = if cfg!(target_os = "macos") {
    "Patina on macOS"
} else if cfg!(windows) {
    "Patina on Windows"
} else {
    "Patina on Linux"
};

const FILE_NAME: &str = "jellyfin.json";

/// A server added to Patina, and the account signed in on it, if any.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    /// The server's own id, which outlives its addresses.
    pub id: String,
    pub name: String,
    /// The base URL requests go under, without a trailing slash.
    pub address: String,
    pub version: String,
    /// Certificates to trust for this server: the CA of its own that
    /// signed its certificate, or its own self-signed certificate, which
    /// has to be given again when the server's certificate is replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<PathBuf>,
    /// Empty while no account is signed in.
    pub user_id: String,
    pub user_name: String,
}

impl Server {
    pub fn url(&self) -> Option<Url> {
        Url::parse(&self.address).ok()
    }

    pub fn signed_in(&self) -> bool {
        !self.user_id.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    device_id: String,
    servers: Vec<Server>,
}

/// `jellyfin.json`, loaded. Every change is saved at once.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    saved: Saved,
}

impl Store {
    /// The file in `data_dir` (`Paths::data`), made with a new device id
    /// the first time.
    ///
    /// A file that is there but can't be read (written by a later Patina,
    /// say) is not replaced at once, as settings are: a new device id would
    /// be a new device on every server, and the servers it lists would be
    /// gone with their tokens left in the keyring. What can be read of it
    /// is kept, it is copied beside itself as `jellyfin.json.bad`, and it
    /// is saved over only when something changes.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE_NAME);
        // Whether the file read as a whole, or isn't there: only then may
        // it be written at once.
        let (mut saved, readable) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Saved>(&bytes) {
                Ok(saved) => (saved, true),
                Err(error) => {
                    log::warn!("could not read {}: {error}", path.display());
                    keep_aside(&path, &bytes);
                    (salvage(&bytes), false)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Saved::default(), true),
            Err(error) => {
                log::warn!("could not read {}: {error}", path.display());
                (Saved::default(), false)
            }
        };
        let new = saved.device_id.trim().is_empty();
        if new {
            saved.device_id = new_device_id();
        }
        let store = Self { path, saved };
        if new && readable {
            store.save();
        }
        store
    }

    /// The file, which the tests read.
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Random, made once per configuration: two installations sharing one
    /// would sign each other out, since signing in revokes the account's
    /// other tokens on the same device id. The tests read it; the app
    /// sends it in the header, through `identity`.
    #[cfg(test)]
    pub fn device_id(&self) -> &str {
        &self.saved.device_id
    }

    pub fn identity(&self) -> Identity {
        Identity::new(DEVICE_NAME, &self.saved.device_id)
    }

    pub fn servers(&self) -> &[Server] {
        &self.saved.servers
    }

    pub fn server(&self, id: &str) -> Option<&Server> {
        self.saved.servers.iter().find(|server| server.id == id)
    }

    /// Adds a server, or replaces the one with the same id where it is in
    /// the list.
    pub fn add(&mut self, server: Server) {
        match self
            .saved
            .servers
            .iter_mut()
            .find(|existing| existing.id == server.id)
        {
            Some(existing) => *existing = server,
            None => self.saved.servers.push(server),
        }
        self.save();
    }

    /// Changes the server with this id; false when there is none.
    pub fn update(&mut self, id: &str, change: impl FnOnce(&mut Server)) -> bool {
        let Some(server) = self.saved.servers.iter_mut().find(|server| server.id == id) else {
            return false;
        };
        change(server);
        self.save();
        true
    }

    fn save(&self) {
        crate::settings::save_json(&self.path, &self.saved);
    }
}

/// A copy of an unreadable `jellyfin.json`, so that nothing in it is lost
/// for good when it is next saved over.
fn keep_aside(path: &Path, bytes: &[u8]) {
    let copy = path.with_extension("json.bad");
    match std::fs::write(&copy, bytes) {
        Ok(()) => log::warn!("kept a copy of it as {}", copy.display()),
        Err(error) => log::warn!("could not keep a copy as {}: {error}", copy.display()),
    }
}

/// What can be read of a `jellyfin.json` that doesn't parse as a whole:
/// the device id, and each server, field by field where it must be. A CA
/// file that can't be read is left out, which makes that server's
/// requests fail rather than trust too much.
fn salvage(bytes: &[u8]) -> Saved {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Saved::default();
    };
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let servers = value
        .get("servers")
        .and_then(serde_json::Value::as_array)
        .map(|servers| {
            servers
                .iter()
                .filter_map(|server| {
                    serde_json::from_value::<Server>(server.clone())
                        .ok()
                        .or_else(|| {
                            let id = text(server, "id");
                            (!id.is_empty()).then(|| Server {
                                id,
                                name: text(server, "name"),
                                address: text(server, "address"),
                                version: text(server, "version"),
                                ca_file: None,
                                user_id: text(server, "user_id"),
                                user_name: text(server, "user_name"),
                            })
                        })
                        .filter(|server| !server.id.is_empty())
                })
                .collect()
        })
        .unwrap_or_default();
    Saved {
        device_id: text(&value, "device_id"),
        servers,
    }
}

/// 16 random bytes as hex.
fn new_device_id() -> String {
    let mut bytes = [0u8; 16];
    if let Err(error) = getrandom::fill(&mut bytes) {
        // Not secret, only unlikely to repeat: std's hasher keys come from
        // the system's random source too, by other means.
        log::warn!("no random bytes for the Jellyfin device id: {error}");
        use std::hash::BuildHasher;
        for (index, chunk) in bytes.chunks_mut(8).enumerate() {
            let value = std::collections::hash_map::RandomState::new().hash_one((
                std::process::id(),
                index,
                std::time::SystemTime::now(),
            ));
            chunk.copy_from_slice(&value.to_le_bytes());
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The token's account name in the system's store.
pub fn token_account(server_id: &str, user_id: &str) -> String {
    format!("{server_id}/{user_id}")
}

#[cfg(test)]
use self::memory as secrets;
/// Where tokens are kept: the system's store, or for tests a stand-in in
/// memory, so that no test reads, writes or deletes anything in the keyring
/// of whoever runs it.
#[cfg(not(test))]
use crate::credentials as secrets;

/// The saved token for an account, if there is one. Blocks.
pub fn load_token(server_id: &str, user_id: &str) -> Result<Option<String>, String> {
    secrets::get(TOKEN_SERVICE, &token_account(server_id, user_id))
}

/// Blocks.
pub fn save_token(server_id: &str, user_id: &str, token: &str) -> Result<(), String> {
    secrets::set(TOKEN_SERVICE, &token_account(server_id, user_id), token)
}

/// Blocks; deleting a token that isn't there succeeds.
pub fn delete_token(server_id: &str, user_id: &str) -> Result<(), String> {
    secrets::delete(TOKEN_SERVICE, &token_account(server_id, user_id))
}

/// The tests' keyring, with crate::credentials' calls.
#[cfg(test)]
mod memory {
    use std::collections::HashMap;
    use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

    type Kept = HashMap<(String, String), String>;

    fn kept() -> MutexGuard<'static, Kept> {
        static KEPT: OnceLock<Mutex<Kept>> = OnceLock::new();
        KEPT.get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
        Ok(kept().get(&(service.into(), account.into())).cloned())
    }

    pub fn set(service: &str, account: &str, password: &str) -> Result<(), String> {
        kept().insert((service.into(), account.into()), password.into());
        Ok(())
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        kept().remove(&(service.into(), account.into()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of the test's own, removed when the test ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("patina-jellyfin-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn server(id: &str) -> Server {
        Server {
            id: id.into(),
            name: format!("Server {id}"),
            address: "https://media.example.org".into(),
            version: "12.4.0".into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_device_id_is_made_once_and_kept() {
        let folder = Folder::new("device-id");
        let store = Store::load(&folder.0);
        let id = store.device_id().to_owned();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(store.path().exists());
        assert_eq!(Store::load(&folder.0).device_id(), id);
        let identity = store.identity();
        assert_eq!(identity.device, DEVICE_NAME);
        assert_eq!(identity.device_id, id);
        let other = Folder::new("device-id-other");
        assert_ne!(Store::load(&other.0).device_id(), id);
    }

    #[test]
    fn servers_are_added_changed_and_removed() {
        let folder = Folder::new("servers");
        let mut store = Store::load(&folder.0);
        store.add(server("a"));
        store.add(Server {
            ca_file: Some(PathBuf::from("ca.pem")),
            ..server("b")
        });
        assert!(!store.server("a").unwrap().signed_in());
        assert!(store.update("a", |server| {
            server.user_id = "u1".into();
            server.user_name = "viewer".into();
        }));
        assert!(!store.update("missing", |_| {}));
        // Adding again replaces, in place.
        store.add(Server {
            name: "Renamed".into(),
            ..store.server("b").unwrap().clone()
        });

        let reloaded = Store::load(&folder.0);
        let ids: Vec<&str> = reloaded.servers().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        let a = reloaded.server("a").unwrap();
        assert!(a.signed_in());
        assert_eq!(a.user_name, "viewer");
        assert_eq!(a.url().unwrap().host_str(), Some("media.example.org"));
        let b = reloaded.server("b").unwrap();
        assert_eq!(b.name, "Renamed");
        assert_eq!(b.ca_file.as_deref(), Some(Path::new("ca.pem")));
    }

    #[test]
    fn the_file_holds_no_token_and_tolerates_missing_fields() {
        let folder = Folder::new("file");
        std::fs::create_dir_all(&folder.0).unwrap();
        std::fs::write(
            folder.0.join(FILE_NAME),
            r#"{"device_id": "abc123", "servers": [{"id": "s", "address": "http://nas:8096", "extra": 1}]}"#,
        )
        .unwrap();
        let mut store = Store::load(&folder.0);
        assert_eq!(store.device_id(), "abc123");
        assert_eq!(store.servers()[0].user_id, "");
        store.update("s", |server| server.user_id = "u".into());
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text.to_ascii_lowercase().contains("token"), "{text}");
        assert!(!text.contains("ca_file"), "{text}");
        assert_eq!(token_account("s", "u"), "s/u");
    }

    #[test]
    fn an_unreadable_file_is_kept_until_something_changes() {
        let folder = Folder::new("unreadable");
        std::fs::create_dir_all(&folder.0).unwrap();
        let file = folder.0.join(FILE_NAME);
        // A field of a type this version doesn't know, as a later version
        // might write it.
        let original = r#"{"device_id": "abc123", "servers": [
            {"id": "s1", "name": "Den", "address": "https://media.example.org", "user_id": "u1",
             "user_name": "viewer", "ca_file": 7},
            {"id": "s2", "address": "http://nas:8096"}, {"name": "no id"}], "servers_v2": {}}"#;
        std::fs::write(&file, original).unwrap();

        let mut store = Store::load(&folder.0);
        assert_eq!(store.device_id(), "abc123");
        let ids: Vec<&str> = store.servers().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2"]);
        let den = store.server("s1").unwrap();
        assert!(den.signed_in() && den.user_name == "viewer" && den.ca_file.is_none());
        // Nothing is written until something changes, and a copy is kept.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(folder.0.join("jellyfin.json.bad")).unwrap(),
            original
        );
        store.update("s2", |server| server.name = "Attic".into());
        let reloaded = Store::load(&folder.0);
        assert_eq!(reloaded.device_id(), "abc123");
        assert_eq!(reloaded.servers().len(), 2);

        // Not JSON at all: a device id for this run, and the file kept.
        std::fs::write(&file, "{\"device_id\": \"abc").unwrap();
        let store = Store::load(&folder.0);
        assert_eq!(store.device_id().len(), 32);
        assert!(store.servers().is_empty());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"device_id\": \"abc"
        );
    }
}
