//! Encrypted, atomic device repository. No plaintext profile or password files.
use remoteapp_crypto_store::{EncryptedPayload, VaultKey, decrypt, encrypt};
use remoteapp_rdp_core::{ConnectionProfile, ProfileId};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

use crate::platform::{PlatformServices, atomic_write};

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub profile: ConnectionProfile,
    pub favorite: bool,
    pub remember_password: bool,
    pub password: Option<String>,
    pub direct_touch: bool,
    pub dynamic_resolution: bool,
}
impl Drop for Device {
    fn drop(&mut self) {
        if let Some(password) = &mut self.password {
            password.zeroize();
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub devices: Vec<Device>,
    pub trust: BTreeMap<String, String>,
}
pub fn endpoint_key(profile: &ConnectionProfile) -> String {
    let host = profile.host.trim().trim_end_matches('.');
    let host = host
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| host.to_ascii_lowercase());
    format!("[{host}]:{}", profile.port)
}

pub struct DeviceRepository {
    path: PathBuf,
    key: VaultKey,
    pub catalog: Catalog,
}
impl DeviceRepository {
    #[cfg(test)]
    pub fn for_test(directory: PathBuf) -> Self {
        Self {
            path: directory.join("devices.enc"),
            key: VaultKey::generate().unwrap(),
            catalog: Catalog::default(),
        }
    }
    pub fn open(platform: &dyn PlatformServices) -> Result<Self, String> {
        let directory = platform.data_dir()?;
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path = directory.join("devices.v1.enc");
        let key_path = directory.join("vault-key.protected");
        let key = if key_path.exists() {
            let protected = std::fs::read(&key_path).map_err(|e| e.to_string())?;
            let plaintext = Zeroizing::new(platform.unprotect(&protected)?);
            let bytes = plaintext
                .as_slice()
                .try_into()
                .map_err(|_| "本地密钥损坏")?;
            VaultKey::from_bytes(bytes)
        } else {
            if path.exists() {
                return Err("设备文件存在但安全密钥丢失，未覆盖原数据".into());
            }
            let key = VaultKey::generate().map_err(|e| e.to_string())?;
            atomic_write(&key_path, &platform.protect(key.as_bytes())?)?;
            key
        };
        let catalog = if path.exists() {
            read_catalog(&path, &key)?
        } else {
            Catalog::default()
        };
        Ok(Self { path, key, catalog })
    }
    /// Commit on disk first: failed writes never change the visible catalog.
    pub fn update(&mut self, change: impl FnOnce(&mut Catalog)) -> Result<(), String> {
        let mut next = self.catalog.clone();
        change(&mut next);
        for device in &mut next.devices {
            device.profile.validate().map_err(|e| e.to_string())?;
            if !device.remember_password {
                device.password = None;
            }
        }
        let json = Zeroizing::new(serde_json::to_vec(&next).map_err(|e| e.to_string())?);
        let envelope =
            encrypt(&self.key, b"remoteapp/devices/v1", &json).map_err(|e| e.to_string())?;
        atomic_write(
            &self.path,
            &serde_json::to_vec(&envelope).map_err(|e| e.to_string())?,
        )?;
        self.catalog = next;
        Ok(())
    }
    pub fn device(&self, id: ProfileId) -> Option<&Device> {
        self.catalog.devices.iter().find(|d| d.profile.id == id)
    }
}
fn read_catalog(path: &Path, key: &VaultKey) -> Result<Catalog, String> {
    let file = std::fs::read(path).map_err(|e| e.to_string())?;
    let payload: EncryptedPayload = serde_json::from_slice(&file).map_err(|e| e.to_string())?;
    let plaintext =
        Zeroizing::new(decrypt(key, b"remoteapp/devices/v1", &payload).map_err(|e| e.to_string())?);
    serde_json::from_slice(&plaintext).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repo() -> DeviceRepository {
        let directory = std::env::temp_dir().join(format!("remoteapp-test-{}", ProfileId::new()));
        std::fs::create_dir_all(&directory).unwrap();
        DeviceRepository {
            path: directory.join("devices.enc"),
            key: VaultKey::generate().unwrap(),
            catalog: Catalog::default(),
        }
    }
    fn device(remember_password: bool) -> Device {
        Device {
            profile: ConnectionProfile {
                host: "SERVER.".into(),
                username: "alice".into(),
                ..Default::default()
            },
            favorite: false,
            remember_password,
            password: Some("secret-rdp-password".into()),
            direct_touch: false,
            dynamic_resolution: true,
        }
    }
    #[test]
    fn encrypted_restart_and_password_opt_in() {
        let mut repo = repo();
        repo.update(|c| c.devices.push(device(false))).unwrap();
        assert!(
            read_catalog(&repo.path, &repo.key).unwrap().devices[0]
                .password
                .is_none()
        );
        repo.update(|c| c.devices.push(device(true))).unwrap();
        assert_eq!(
            read_catalog(&repo.path, &repo.key).unwrap().devices[1]
                .password
                .as_deref(),
            Some("secret-rdp-password")
        );
        assert!(
            !String::from_utf8_lossy(&std::fs::read(&repo.path).unwrap())
                .contains("secret-rdp-password")
        );
        std::fs::remove_dir_all(repo.path.parent().unwrap()).unwrap();
    }
    #[test]
    fn failed_write_preserves_catalog() {
        let mut repo = repo();
        repo.path = repo.path.join("missing-parent");
        assert!(repo.update(|c| c.devices.push(device(false))).is_err());
        assert!(repo.catalog.devices.is_empty());
        std::fs::remove_dir_all(repo.path.parent().unwrap().parent().unwrap()).unwrap();
    }
    #[test]
    fn trust_is_scoped_to_normalized_endpoint() {
        let a = device(false).profile.clone();
        let mut b = a.clone();
        b.host = "server".into();
        assert_eq!(endpoint_key(&a), endpoint_key(&b));
        b.port += 1;
        assert_ne!(endpoint_key(&a), endpoint_key(&b));
    }
}
