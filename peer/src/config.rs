use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;
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
    /// ID único desta máquina — gerado automaticamente na primeira execução
    #[serde(default)]
    pub machine_id:          String,
    /// ID do host remoto que o usuário quer acessar
    #[serde(default)]
    pub remote_id:           String,
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
            machine_id:          Self::generate_id(),
            remote_id:           String::new(),
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".remote-link.json")
    }

    /// Gera um ID curto legível tipo "A7X-92K"
    fn generate_id() -> String {
        let id = Uuid::new_v4().to_string();
        let parts: Vec<&str> = id.split('-').collect();
        format!("{}-{}", &parts[0][..3].to_uppercase(), &parts[1][..3].to_uppercase())
    }

    pub fn load() -> Self {
        let p = Self::path();
        if p.exists() {
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Ok(mut c) = serde_json::from_str::<Config>(&s) {
                    // Garante que machine_id existe (migração de configs antigas)
                    if c.machine_id.is_empty() {
                        c.machine_id = Self::generate_id();
                        c.save();
                    }
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

    /// Retorna (relay_host, relay_port) se relay habilitado e configurado
    pub fn relay(&self) -> Option<(String, u16)> {
        if self.use_relay && !self.relay_host.is_empty() {
            Some((self.relay_host.clone(), self.relay_port))
        } else {
            None
        }
    }

    /// ID desta máquina no relay
    pub fn relay_id_host(&self) -> String {
        self.machine_id.clone()
    }
}
