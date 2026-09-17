//! Secret storage: the OS credential store by default, an in-memory store for tests, and a
//! plain-file fallback for machines with no usable credential store.

use crate::{AuthError, Result};
use base64::Engine as _;
use cobalt_core::{Secret, SecretRef, KEYRING_SERVICE};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Key/value storage for secrets referenced by [`SecretRef`]. Implementations must be cheap to
/// call from any thread; the resolver never holds one across an `.await`.
pub trait SecretStore: Send + Sync {
    fn get(&self, r: &SecretRef) -> Result<Option<Secret>>;
    fn set(&self, r: &SecretRef, s: &Secret) -> Result<()>;
    fn delete(&self, r: &SecretRef) -> Result<()>;
}

// ---------------------------------------------------------------------------------------------
// OS credential store

/// Windows Credential Manager / macOS Keychain / Linux Secret Service, via `keyring`.
///
/// Entries live under service [`KEYRING_SERVICE`] with the `SecretRef::key` as the user field.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }

    /// Whether the platform credential store initialised. On Linux this is `false` when no
    /// Secret Service provider (gnome-keyring, KWallet, KeePassXC, ...) is running on the bus.
    pub fn available() -> bool {
        keyring::Entry::store_status().is_ok()
    }

    /// Why the store is unavailable, if it is.
    pub fn unavailable_reason() -> Option<String> {
        keyring::Entry::store_status()
            .as_ref()
            .err()
            .map(|e| e.to_string())
    }

    fn entry(r: &SecretRef) -> Result<keyring::Entry> {
        if let Some(reason) = Self::unavailable_reason() {
            return Err(AuthError::SecretStoreUnavailable(reason));
        }
        keyring::Entry::new(KEYRING_SERVICE, &r.key).map_err(map_keyring)
    }
}

fn map_keyring(e: keyring::Error) -> AuthError {
    use keyring::Error as K;
    match e {
        K::NoDefaultStore | K::NoStorageAccess(_) | K::NotSupportedByStore(_) => {
            AuthError::SecretStoreUnavailable(e.to_string())
        }
        other => AuthError::Secret(other.to_string()),
    }
}

/// Windows Credential Manager caps a secret blob at 2560 bytes (1280 UTF-16 chars); Entra refresh tokens are
/// longer. Values above this are split into `key#0`, `key#1`, … with a header in `key`.
const CHUNK_CHARS: usize = 1000;
const CHUNK_HEADER: &str = "__cobalt_chunks:";

fn chunk_ref(r: &SecretRef, i: usize) -> SecretRef {
    SecretRef { key: format!("{}#{i}", r.key) }
}

impl SecretStore for KeyringStore {
    fn get(&self, r: &SecretRef) -> Result<Option<Secret>> {
        let head = match Self::entry(r)?.get_password() {
            Ok(p) => p,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(e) => return Err(map_keyring(e)),
        };
        if let Some(n) = head.strip_prefix(CHUNK_HEADER).and_then(|n| n.parse::<usize>().ok()) {
            let mut out = String::new();
            for i in 0..n {
                match Self::entry(&chunk_ref(r, i))?.get_password() {
                    Ok(p) => out.push_str(&p),
                    Err(keyring::Error::NoEntry) => return Ok(None),
                    Err(e) => return Err(map_keyring(e)),
                }
            }
            return Ok(Some(Secret::new(out)));
        }
        Ok(Some(Secret::new(head)))
    }

    fn set(&self, r: &SecretRef, s: &Secret) -> Result<()> {
        let value = s.expose();
        let chars: Vec<char> = value.chars().collect();
        if chars.len() <= CHUNK_CHARS {
            let _ = self.delete_chunks(r);
            return Self::entry(r)?.set_password(value).map_err(map_keyring);
        }
        let chunks: Vec<String> = chars.chunks(CHUNK_CHARS).map(|c| c.iter().collect()).collect();
        for (i, c) in chunks.iter().enumerate() {
            Self::entry(&chunk_ref(r, i))?.set_password(c).map_err(map_keyring)?;
        }
        Self::entry(r)?.set_password(&format!("{CHUNK_HEADER}{}", chunks.len())).map_err(map_keyring)
    }

    fn delete(&self, r: &SecretRef) -> Result<()> {
        let _ = self.delete_chunks(r);
        match Self::entry(r)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_keyring(e)),
        }
    }
}

impl KeyringStore {
    fn delete_chunks(&self, r: &SecretRef) -> Result<()> {
        if let Ok(head) = Self::entry(r)?.get_password() {
            if let Some(n) = head.strip_prefix(CHUNK_HEADER).and_then(|n| n.parse::<usize>().ok()) {
                for i in 0..n {
                    let _ = Self::entry(&chunk_ref(r, i))?.delete_credential();
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// In-memory store (tests, and a "remember nothing" mode)

#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<String, Secret>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn contains(&self, r: &SecretRef) -> bool {
        self.map.lock().unwrap().contains_key(&r.key)
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, r: &SecretRef) -> Result<Option<Secret>> {
        Ok(self.map.lock().unwrap().get(&r.key).cloned())
    }
    fn set(&self, r: &SecretRef, s: &Secret) -> Result<()> {
        self.map.lock().unwrap().insert(r.key.clone(), s.clone());
        Ok(())
    }
    fn delete(&self, r: &SecretRef) -> Result<()> {
        self.map.lock().unwrap().remove(&r.key);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// File fallback

/// A JSON file of base64-encoded secrets. **Not encrypted**: base64 is obfuscation only.
/// Used solely when the OS store is unavailable; every construction and write logs a warning.
#[derive(Debug)]
pub struct FileFallbackStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileFallbackStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        tracing::warn!(
            path = %path.display(),
            "OS credential store unavailable: secrets will be stored base64-encoded in a plain file. Anyone who can read this file can read your passwords and refresh tokens."
        );
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// `<data dir>/secrets.json` (Windows: `%APPDATA%/Cobalt/Cobalt SQL Works/data/secrets.json`;
    /// Linux: `~/.local/share/cobaltsqlworks/secrets.json`).
    pub fn in_data_dir() -> Result<Self> {
        let dirs = directories::ProjectDirs::from(
            cobalt_core::APP_QUALIFIER,
            cobalt_core::APP_ORG,
            cobalt_core::APP_NAME,
        )
        .ok_or_else(|| {
            AuthError::Other(
                "no home directory; cannot locate a data dir for the secrets file".into(),
            )
        })?;
        let dir = dirs.data_dir();
        std::fs::create_dir_all(dir)
            .map_err(|e| AuthError::Secret(format!("create {}: {e}", dir.display())))?;
        Ok(Self::new(dir.join("secrets.json")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(BTreeMap::new()),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| AuthError::Secret(format!("{}: {e}", self.path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(AuthError::Secret(format!(
                "read {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<()> {
        let json = serde_json::to_vec_pretty(map).map_err(|e| AuthError::Secret(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .map_err(|e| AuthError::Secret(format!("write {}: {e}", tmp.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| AuthError::Secret(format!("rename to {}: {e}", self.path.display())))
    }
}

impl SecretStore for FileFallbackStore {
    fn get(&self, r: &SecretRef) -> Result<Option<Secret>> {
        let _g = self.lock.lock().unwrap();
        let map = self.load()?;
        match map.get(&r.key) {
            None => Ok(None),
            Some(b64) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| AuthError::Secret(format!("corrupt entry {}: {e}", r.key)))?;
                let s = String::from_utf8(bytes).map_err(|_| {
                    AuthError::Secret(format!("corrupt entry {}: not UTF-8", r.key))
                })?;
                Ok(Some(Secret::new(s)))
            }
        }
    }

    fn set(&self, r: &SecretRef, s: &Secret) -> Result<()> {
        tracing::warn!(key = %r.key, "storing a secret in the plain-file fallback store");
        let _g = self.lock.lock().unwrap();
        let mut map = self.load()?;
        map.insert(
            r.key.clone(),
            base64::engine::general_purpose::STANDARD.encode(s.expose()),
        );
        self.save(&map)
    }

    fn delete(&self, r: &SecretRef) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.load()?;
        if map.remove(&r.key).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }
}

/// The store the app should use: the OS store when it works, otherwise the file fallback
/// (with a warning), or an error if even that cannot be set up.
pub fn default_secret_store() -> Result<Arc<dyn SecretStore>> {
    if KeyringStore::available() {
        Ok(Arc::new(KeyringStore))
    } else {
        tracing::warn!(reason = ?KeyringStore::unavailable_reason(), "falling back to the plain-file secret store");
        Ok(Arc::new(FileFallbackStore::in_data_dir()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cobalt_core::ProfileId;

    #[test]
    fn memory_store_roundtrip() {
        let store = MemoryStore::new();
        let r = SecretRef::for_profile(&ProfileId::new(), "password");
        assert_eq!(store.get(&r).unwrap(), None);
        store.set(&r, &Secret::new("hunter2")).unwrap();
        assert_eq!(store.get(&r).unwrap().unwrap().expose(), "hunter2");
        store.delete(&r).unwrap();
        assert_eq!(store.get(&r).unwrap(), None);
        store.delete(&r).unwrap(); // idempotent
    }

    #[test]
    fn file_fallback_roundtrip_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let r = SecretRef::for_profile(&ProfileId::new(), "refresh_token");
        {
            let store = FileFallbackStore::new(&path);
            assert_eq!(store.get(&r).unwrap(), None);
            store.set(&r, &Secret::new("tok/with+chars=")).unwrap();
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("tok/with+chars="),
            "secret must not be stored verbatim"
        );
        let store = FileFallbackStore::new(&path);
        assert_eq!(store.get(&r).unwrap().unwrap().expose(), "tok/with+chars=");
        store.delete(&r).unwrap();
        assert_eq!(store.get(&r).unwrap(), None);
    }

    #[test]
    fn file_fallback_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileFallbackStore::new(dir.path().join("nope.json"));
        assert_eq!(store.get(&SecretRef { key: "x".into() }).unwrap(), None);
        store.delete(&SecretRef { key: "x".into() }).unwrap();
    }
}
