use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};

const KEYRING_SERVICE: &str = "io.github.xuying1128.xiaoli.relay";
const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;

/// API credentials are memory-only unless the user explicitly requests the
/// operating-system credential store. No file or SQLite fallback exists.
pub struct CredentialStore {
    memory: Mutex<HashMap<String, MemoryCredential>>,
}

struct MemoryCredential {
    binding: String,
    secret: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSaveOutcome {
    pub persisted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self {
            memory: Mutex::new(HashMap::new()),
        }
    }
}

impl CredentialStore {
    pub fn save(
        &self,
        profile_id: &str,
        binding: &str,
        credential: &str,
        persist: bool,
    ) -> Result<CredentialSaveOutcome, String> {
        validate_profile_id(profile_id)?;
        validate_binding(binding)?;
        validate_credential(credential)?;
        if persist {
            let reference = new_credential_reference(profile_id, binding)?;
            let account = credential_account_from_reference(profile_id, Some(&reference))
                .ok_or_else(|| "failed to create credential reference".to_owned())?;
            match keyring::Entry::new(KEYRING_SERVICE, &account)
                .and_then(|entry| entry.set_password(credential))
            {
                Ok(()) => {
                    self.clear_memory_binding(profile_id, binding)?;
                    return Ok(CredentialSaveOutcome {
                        persisted: true,
                        credential_ref: Some(reference),
                        warning: None,
                    });
                }
                Err(_) => {
                    self.put_memory(profile_id, binding, credential)?;
                    return Ok(CredentialSaveOutcome {
                        persisted: false,
                        credential_ref: None,
                        warning: Some(
                            "系统凭据库不可用；本次只保存在进程内存，退出后失效".to_owned(),
                        ),
                    });
                }
            }
        }
        self.put_memory(profile_id, binding, credential)?;
        Ok(CredentialSaveOutcome {
            persisted: false,
            credential_ref: None,
            warning: None,
        })
    }

    pub fn get(
        &self,
        profile_id: &str,
        binding: &str,
        credential_ref: Option<&str>,
    ) -> Result<Option<String>, String> {
        validate_profile_id(profile_id)?;
        validate_binding(binding)?;
        let memory_key = memory_key(profile_id, binding);
        if let Some(value) = self
            .memory
            .lock()
            .map_err(|_| "credential memory lock poisoned".to_owned())?
            .get(&memory_key)
            .filter(|value| value.binding == binding)
            .map(|value| value.secret.clone())
        {
            return Ok(Some(value));
        }
        let Some(reference) = credential_ref else {
            return Ok(None);
        };
        if !credential_reference_matches(profile_id, binding, reference) {
            return Err("invalid credential reference".to_owned());
        }
        let account = credential_account_from_reference(profile_id, Some(reference))
            .ok_or_else(|| "invalid credential reference".to_owned())?;
        match keyring::Entry::new(KEYRING_SERVICE, &account).and_then(|entry| entry.get_password())
        {
            Ok(value) => {
                validate_credential(&value)?;
                Ok(Some(value))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err("system credential store unavailable".to_owned()),
        }
    }

    pub fn delete(&self, profile_id: &str, credential_ref: Option<&str>) -> Result<(), String> {
        validate_profile_id(profile_id)?;
        self.clear_memory(profile_id)?;
        self.delete_persisted(profile_id, credential_ref)
    }

    pub fn clear_memory(&self, profile_id: &str) -> Result<(), String> {
        validate_profile_id(profile_id)?;
        let prefix = format!("{profile_id}\u{1f}");
        self.memory
            .lock()
            .map_err(|_| "credential memory lock poisoned".to_owned())?
            .retain(|key, _| !key.starts_with(&prefix));
        Ok(())
    }

    pub fn clear_memory_binding(&self, profile_id: &str, binding: &str) -> Result<(), String> {
        validate_profile_id(profile_id)?;
        validate_binding(binding)?;
        self.memory
            .lock()
            .map_err(|_| "credential memory lock poisoned".to_owned())?
            .remove(&memory_key(profile_id, binding));
        Ok(())
    }

    pub fn delete_persisted(
        &self,
        profile_id: &str,
        credential_ref: Option<&str>,
    ) -> Result<(), String> {
        validate_profile_id(profile_id)?;
        if let Some(account) = credential_account_from_reference(profile_id, credential_ref) {
            match keyring::Entry::new(KEYRING_SERVICE, &account)
                .and_then(|entry| entry.delete_credential())
            {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(_) => return Err("system credential store unavailable".to_owned()),
            }
        }
        Ok(())
    }

    fn put_memory(&self, profile_id: &str, binding: &str, credential: &str) -> Result<(), String> {
        self.memory
            .lock()
            .map_err(|_| "credential memory lock poisoned".to_owned())?
            .insert(
                memory_key(profile_id, binding),
                MemoryCredential {
                    binding: binding.to_owned(),
                    secret: credential.to_owned(),
                },
            );
        Ok(())
    }
}

fn memory_key(profile_id: &str, binding: &str) -> String {
    format!("{profile_id}\u{1f}{binding}")
}

fn new_credential_reference(profile_id: &str, binding: &str) -> Result<String, String> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| "operating-system random source unavailable")?;
    let nonce = nonce
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "keyring:{profile_id}:{:016x}:{nonce}",
        fnv1a64(binding.as_bytes())
    ))
}

fn credential_reference_matches(profile_id: &str, binding: &str, reference: &str) -> bool {
    let prefix = format!("keyring:{profile_id}:{:016x}", fnv1a64(binding.as_bytes()));
    reference == prefix
        || reference
            .strip_prefix(&(prefix + ":"))
            .is_some_and(|nonce| {
                nonce.len() == 16 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

fn credential_account_from_reference(
    profile_id: &str,
    credential_ref: Option<&str>,
) -> Option<String> {
    let reference = credential_ref?;
    let prefix = format!("keyring:{profile_id}:");
    let suffix = reference.strip_prefix(&prefix)?;
    let mut parts = suffix.split(':');
    let digest = parts.next()?;
    let nonce = parts.next();
    if parts.next().is_some()
        || digest.len() != 16
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || nonce.is_some_and(|value| {
            value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return None;
    }
    Some(match nonce {
        Some(value) => format!("{profile_id}:{digest}:{value}"),
        None => format!("{profile_id}:{digest}"),
    })
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn validate_profile_id(profile_id: &str) -> Result<(), String> {
    if profile_id.is_empty()
        || profile_id.len() > 128
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("invalid relay profile id".to_owned());
    }
    Ok(())
}

fn validate_credential(credential: &str) -> Result<(), String> {
    if credential.trim().is_empty() || credential.len() > MAX_CREDENTIAL_BYTES {
        return Err("credential is empty or too large".to_owned());
    }
    Ok(())
}

fn validate_binding(binding: &str) -> Result<(), String> {
    if binding.trim().is_empty() || binding.len() > 4_096 {
        return Err("credential binding is empty or too large".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_credentials_never_create_a_reference() {
        let store = CredentialStore::default();
        let outcome = store
            .save(
                "relay-one",
                "responses|https://one.example/v1",
                "sk-private",
                false,
            )
            .unwrap();
        assert!(!outcome.persisted);
        assert!(outcome.credential_ref.is_none());
        assert_eq!(
            store
                .get("relay-one", "responses|https://one.example/v1", None)
                .unwrap()
                .as_deref(),
            Some("sk-private")
        );
        store.delete("relay-one", None).unwrap();
        assert!(store
            .get("relay-one", "responses|https://one.example/v1", None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn invalid_reference_cannot_select_another_profiles_secret() {
        let store = CredentialStore::default();
        assert_eq!(
            store
                .get(
                    "relay-one",
                    "responses|https://one.example/v1",
                    Some("keyring:relay-two:0123456789abcdef"),
                )
                .unwrap_err(),
            "invalid credential reference"
        );
    }

    #[test]
    fn memory_credential_is_bound_to_endpoint_and_protocol() {
        let store = CredentialStore::default();
        store
            .save(
                "relay-one",
                "responses|https://one.example/v1",
                "sk-private",
                false,
            )
            .unwrap();
        assert!(store
            .get("relay-one", "chat|https://one.example/v1", None)
            .unwrap()
            .is_none());
        assert!(store
            .get("relay-one", "responses|https://two.example/v1", None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn versioned_references_are_bound_and_parseable() {
        let reference =
            new_credential_reference("relay-one", "responses|https://one.example/v1").unwrap();
        assert!(credential_reference_matches(
            "relay-one",
            "responses|https://one.example/v1",
            &reference
        ));
        assert!(!credential_reference_matches(
            "relay-one",
            "responses|https://two.example/v1",
            &reference
        ));
        assert!(credential_account_from_reference("relay-one", Some(&reference)).is_some());
    }

    #[test]
    fn memory_bindings_can_coexist_during_a_profile_update() {
        let store = CredentialStore::default();
        store
            .save(
                "relay-one",
                "responses|https://one.example/v1",
                "sk-old",
                false,
            )
            .unwrap();
        store
            .save(
                "relay-one",
                "responses|https://two.example/v1",
                "sk-new",
                false,
            )
            .unwrap();
        assert_eq!(
            store
                .get("relay-one", "responses|https://one.example/v1", None)
                .unwrap()
                .as_deref(),
            Some("sk-old")
        );
        assert_eq!(
            store
                .get("relay-one", "responses|https://two.example/v1", None)
                .unwrap()
                .as_deref(),
            Some("sk-new")
        );
    }
}
