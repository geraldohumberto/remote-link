use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::protocol::{DEFAULT_PASSWORD, DEFAULT_PEER_PORT, DEFAULT_RELAY_PORT, FPS_TARGET, JPEG_QUALITY};

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
    // Relay
    pub use_relay:           bool,
    pub relay_host:          String,
    pub relay_port:          u16,
    /// ID único deste host no relay (ex: "pc-empresa", "home-humberto")
    /// Se vazio, usa relay_host:port como fallback
    #[serde(default)]
    pub relay_id:            String,
}

impl Default for Config {
    fn default() -> Self {
        let shared = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("RemoteLink_Shared")
            .to_string_lossy().to_string();
        Self {
            password:            DEFAULT_PASSWORD.to_string(),
            port:                DEFAULT_PEER_PORT,
            jpeg_quality:        JPEG_QUALITY,
            fps:                 FPS_TARGET,
            allow_file_transfer: true,
            shared_folder:       shared,
            last_host:           String::new(),
            last_port:           DEFAULT_PEER_PORT,
            first_run:           true,
            use_relay:           false,
            relay_host:          String::new(),
            relay_port:          DEFAULT_RELAY_PORT,
            relay_id:            String::new(),
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".remote-link.json")
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
        c.save();
        c
    }

    pub fn save(&self) {
        if let Ok(j) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), j);
        }
    }

    pub fn shared_path(&self) -> PathBuf {
        PathBuf::from(&self.shared_folder)
    }

    /// Retorna (relay_host, relay_port) se o relay estiver habilitado e configurado.
    pub fn relay(&self) -> Option<(String, u16)> {
        if self.use_relay && !self.relay_host.is_empty() {
            Some((self.relay_host.clone(), self.relay_port))
        } else {
            None
        }
    }

    /// ID que identifica este host no relay.
    /// Se `relay_id` estiver preenchido, usa ele.
    /// Caso contrário, usa o hostname da máquina.
    /// O cliente vai usar esse ID para conectar via relay.
    pub fn relay_id_host(&self) -> String {
        if !self.relay_id.is_empty() {
            return self.relay_id.clone();
        }
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "remotelink-host".to_string())
    }
}
