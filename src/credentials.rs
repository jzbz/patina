//! Passwords kept in the system's own store rather than in Patina's
//! settings: the Secret Service on Linux (GNOME Keyring, KWallet), the
//! Keychain on macOS, Credential Manager on Windows. Calls can block, and
//! the store may ask to be unlocked, so they belong on a thread other than
//! the interface's.

use std::sync::OnceLock;

/// What the store is called where Patina mentions it.
pub const STORE_NAME: &str = if cfg!(target_os = "macos") {
    "the Keychain"
} else if cfg!(windows) {
    "Credential Manager"
} else {
    "your keyring"
};

/// Makes the platform's store the one entries go to, the first time.
fn store() -> Result<(), String> {
    static STORE: OnceLock<Result<(), String>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            #[cfg(all(unix, not(target_os = "macos")))]
            let store = zbus_secret_service_keyring_store::Store::new();
            #[cfg(target_os = "macos")]
            let store = apple_native_keyring_store::keychain::Store::new();
            #[cfg(windows)]
            let store = windows_native_keyring_store::Store::new();
            match store {
                Ok(store) => {
                    keyring_core::set_default_store(store);
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            }
        })
        .clone()
}

fn entry(service: &str, account: &str) -> Result<keyring_core::Entry, String> {
    store()?;
    keyring_core::Entry::new(service, account).map_err(|error| error.to_string())
}

/// The password saved for `account`, if there is one.
pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
    match entry(service, account)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub fn set(service: &str, account: &str, password: &str) -> Result<(), String> {
    entry(service, account)?
        .set_password(password)
        .map_err(|error| error.to_string())
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    match entry(service, account)?.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
