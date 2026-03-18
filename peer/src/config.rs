use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::protocol::{DEFAULT_PASSWORD, DEFAULT_PEER_PORT, FPS_TARGET, JPEG_QUALITY};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub password:            String,
    pub port:                u16,
    pub jpeg_quality:        u8,
    pub fps:                 u64,
    pub allow_file_transfer: bool,
    pub shared_folder:       String,
    pub last_host:           String,
    pub last_port:           u16,
    pub first_run:           bool,
}

impl Default for Config {
    fn default() -> Self {
        let shared = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
            .join("RemoteLink_Shared").to_string_lossy().to_string();
        Self {
            password: DEFAULT_PASSWORD.to_string(), port: DEFAULT_PEER_PORT,
            jpeg_quality: JPEG_QUALITY, fps: FPS_TARGET,
            allow_file_transfer: true, shared_folder: shared,
            last_host: String::new(), last_port: DEFAULT_PEER_PORT, first_run: true,
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".remote-link.json")
    }
    pub fn load() -> Self {
        let p = Self::path();
        if p.exists() {
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Ok(c) = serde_json::from_str::<Config>(&s) {
                    let _ = std::fs::create_dir_all(&c.shared_folder);
                    return c;
                }
            }
        }
        let c = Config::default();
        let _ = std::fs::create_dir_all(&c.shared_folder);
        c.save(); c
    }
    pub fn save(&self) {
        if let Ok(j) = serde_json::to_string_pretty(self) { let _ = std::fs::write(Self::path(), j); }
    }
    pub fn shared_path(&self) -> PathBuf { PathBuf::from(&self.shared_folder) }
}
