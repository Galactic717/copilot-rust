use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine;

use crate::redact::{register_secret, unregister_secret};
use crate::settings::write_json_atomic;

/// DPAPI (CryptProtectData) under the current Windows user: a copy of the file is useless on another account or machine.
mod dpapi {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
        v
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(plain);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).map_err(|e| e.to_string())?;
            Ok(take(out))
        }
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).map_err(|e| e.to_string())?;
            Ok(take(out))
        }
    }
}

/// I never let keys touch settings.json or SQLite. The file holds only DPAPI blobs.
pub struct SecretStore {
    path: PathBuf,
    blobs: Mutex<BTreeMap<String, String>>,
}

impl SecretStore {
    pub fn open(path: PathBuf) -> Self {
        let mut blobs: BTreeMap<String, String> =
            std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        // An entry that no longer decrypts (other user, corrupted) is dropped rather than shown as a saved key.
        blobs.retain(|_, b| decrypt(b).is_some());
        for b in blobs.values() {
            if let Some(v) = decrypt(b) {
                register_secret(&v);
            }
        }
        Self { path, blobs: Mutex::new(blobs) }
    }

    pub fn available(&self) -> bool {
        dpapi::protect(b"probe").and_then(|b| dpapi::unprotect(&b)).map(|v| v == b"probe").unwrap_or(false)
    }

    pub fn has(&self, id: &str) -> bool {
        self.blobs.lock().unwrap().contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<String> {
        self.blobs.lock().unwrap().get(id).and_then(|b| decrypt(b))
    }

    pub fn set(&self, id: &str, value: &str) -> Result<(), String> {
        let enc = dpapi::protect(value.as_bytes())
            .map_err(|_| crate::i18n::t("Системне шифрування (DPAPI) недоступне — ключ не збережено", "System encryption (DPAPI) is unavailable — key not saved"))?;
        let mut blobs = self.blobs.lock().unwrap();
        if let Some(old) = blobs.get(id).and_then(|b| decrypt(b)) {
            unregister_secret(&old);
        }
        blobs.insert(id.to_string(), base64::engine::general_purpose::STANDARD.encode(enc));
        register_secret(value);
        let _ = write_json_atomic(&self.path, &*blobs);
        Ok(())
    }

    pub fn delete(&self, id: &str) {
        let mut blobs = self.blobs.lock().unwrap();
        if let Some(old) = blobs.remove(id).and_then(|b| decrypt(&b)) {
            unregister_secret(&old);
        }
        let _ = write_json_atomic(&self.path, &*blobs);
    }
}

fn decrypt(b64: &str) -> Option<String> {
    let data = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    String::from_utf8(dpapi::unprotect(&data).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_redaction() {
        let p = std::env::temp_dir().join(format!("copilot-sec-{}.json", crate::types::new_id()));
        let s = SecretStore::open(p.clone());
        s.set("provider:x", "sk-test-1234567890abcdef").unwrap();
        assert_eq!(SecretStore::open(p.clone()).get("provider:x").as_deref(), Some("sk-test-1234567890abcdef"));
        assert!(!std::fs::read_to_string(&p).unwrap().contains("sk-test"));
        assert!(!crate::redact::redact("x sk-test-1234567890abcdef").contains("1234567890abcdef"));
        s.delete("provider:x");
        assert!(!s.has("provider:x"));
    }
}
