use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use pbkdf2::pbkdf2_hmac;
use rand::{thread_rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fs;
use std::path::PathBuf;

use crate::paths::data_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultItem {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub note: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VaultData {
    salt: Vec<u8>,
    items: Vec<VaultItem>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedVault {
    salt: String,       // hex encoded
    nonce: String,      // hex encoded
    ciphertext: String, // hex encoded
}

pub struct VaultManager {
    master_key: Option<Key<Aes256Gcm>>,
    salt: Option<[u8; 16]>,
}

impl Default for VaultManager {
    fn default() -> Self {
        Self::new()
    }
}

impl VaultManager {
    pub fn new() -> Self {
        Self {
            master_key: None,
            salt: None,
        }
    }

    pub fn is_unlocked(&self) -> bool {
        self.master_key.is_some()
    }

    fn vault_path() -> Result<PathBuf, String> {
        let mut path = data_dir().map_err(|e| format!("Could not get data dir: {}", e))?;
        path.push("vault.enc");
        Ok(path)
    }

    pub fn unlock(&mut self, password: &str) -> Result<bool, String> {
        let path = Self::vault_path()?;

        // If file doesn't exist, we're setting up a new vault.
        // We'll generate a salt and test encryption immediately to verify.
        if !path.exists() {
            let mut salt = [0u8; 16];
            thread_rng().fill_bytes(&mut salt);
            let key = Self::derive_key(password, &salt);
            self.master_key = Some(key);
            self.salt = Some(salt);

            // Save an empty vault to lock in the password.
            self.save_items(Vec::new())?;
            return Ok(true);
        }

        let encrypted_data =
            fs::read_to_string(&path).map_err(|e| format!("Could not read vault: {}", e))?;

        let vault: EncryptedVault = serde_json::from_str(&encrypted_data)
            .map_err(|e| format!("Could not parse vault: {}", e))?;

        let salt = hex::decode(&vault.salt).map_err(|_| "Invalid salt format".to_string())?;
        let nonce_bytes =
            hex::decode(&vault.nonce).map_err(|_| "Invalid nonce format".to_string())?;
        let ciphertext =
            hex::decode(&vault.ciphertext).map_err(|_| "Invalid ciphertext format".to_string())?;

        let key = Self::derive_key(password, &salt);
        let cipher = Aes256Gcm::new(&key);
        let nonce = Nonce::from_slice(&nonce_bytes);

        match cipher.decrypt(nonce, ciphertext.as_ref()) {
            Ok(_) => {
                self.master_key = Some(key);
                self.salt = Some(
                    salt.try_into()
                        .map_err(|_| "Invalid salt length".to_string())?,
                );
                Ok(true) // Decryption successful
            }
            Err(_) => Ok(false), // Incorrect password
        }
    }

    pub fn lock(&mut self) {
        self.master_key = None;
        self.salt = None;
    }

    pub fn list_items(&self) -> Result<Vec<VaultItem>, String> {
        let key = self.master_key.as_ref().ok_or("Vault is locked")?;
        let path = Self::vault_path()?;

        if !path.exists() {
            return Ok(Vec::new());
        }

        let encrypted_data =
            fs::read_to_string(&path).map_err(|e| format!("Could not read vault: {}", e))?;

        let vault: EncryptedVault = serde_json::from_str(&encrypted_data)
            .map_err(|e| format!("Could not parse vault: {}", e))?;

        let nonce_bytes =
            hex::decode(&vault.nonce).map_err(|_| "Invalid nonce format".to_string())?;
        let ciphertext =
            hex::decode(&vault.ciphertext).map_err(|_| "Invalid ciphertext format".to_string())?;

        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let decrypted = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| "Failed to decrypt vault content".to_string())?;

        let data: VaultData = serde_json::from_slice(&decrypted)
            .map_err(|e| format!("Failed to parse decrypted vault: {}", e))?;

        Ok(data.items)
    }

    pub fn add_item(
        &self,
        name: String,
        secret: String,
        note: String,
    ) -> Result<Vec<VaultItem>, String> {
        let mut items = self.list_items()?;

        let id = format!(
            "vault-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        items.push(VaultItem {
            id,
            name,
            secret,
            note,
        });

        self.save_items(items.clone())?;
        Ok(items)
    }

    pub fn delete_item(&self, id: &str) -> Result<Vec<VaultItem>, String> {
        let mut items = self.list_items()?;
        items.retain(|item| item.id != id);
        self.save_items(items.clone())?;
        Ok(items)
    }

    fn save_items(&self, items: Vec<VaultItem>) -> Result<(), String> {
        let key = self.master_key.as_ref().ok_or("Vault is locked")?;
        let salt = self.salt.ok_or("Salt missing")?;
        let path = Self::vault_path()?;

        let data = VaultData {
            salt: salt.to_vec(),
            items,
        };

        let json = serde_json::to_vec(&data)
            .map_err(|e| format!("Could not serialize vault data: {}", e))?;

        let cipher = Aes256Gcm::new(key);
        let mut nonce_bytes = [0u8; 12];
        thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, json.as_ref())
            .map_err(|_| "Could not encrypt vault data".to_string())?;

        let encrypted_vault = EncryptedVault {
            salt: hex::encode(&salt),
            nonce: hex::encode(nonce_bytes),
            ciphertext: hex::encode(ciphertext),
        };

        let final_json = serde_json::to_string_pretty(&encrypted_vault)
            .map_err(|e| format!("Could not serialize encrypted vault: {}", e))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Could not create vault dir: {}", e))?;
        }

        fs::write(path, final_json).map_err(|e| format!("Could not save vault: {}", e))?;
        Ok(())
    }

    fn derive_key(password: &str, salt: &[u8]) -> Key<Aes256Gcm> {
        let mut key = [0u8; 32];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, 100_000, &mut key);
        *Key::<Aes256Gcm>::from_slice(&key)
    }
}
