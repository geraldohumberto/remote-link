// remote-link-relay/src/main.rs
// Servidor de ponte TCP — roda na VM/servidor com IP público.
// Peers se registram com um ID e o relay conecta dois peers pelo mesmo ID.
//
// Protocolo:
//   → {"action":"register","id":"MEU_ID"}\n   — registra e espera par
//   ← {"ok":true,"id":"MEU_ID"}\n              — confirmação
//   → {"action":"connect","id":"MEU_ID"}\n     — conecta ao par registrado
//   ← {"ok":true}\n                            — confirmação (ambos)
//   Depois: tráfego TCP raw bidirecional

use std::collections::HashMap;
use std::sync::Arc;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;
use serde::{Deserialize, Serialize};

type PeerMap = Arc<Mutex<HashMap<String, TcpStream>>>;

#[derive(Debug, Deserialize)]
struct PeerMsg {
    action: String,
    id:     Option<String>,
}

#[derive(Debug, Serialize)]
struct PeerResp {
    ok:     bool,
    id:     Option<String>,
    reason: Option<String>,
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".remote-link-relay.json")
}

fn load_port() -> u16 {
    #[derive(Deserialize)]
    struct Cfg { port: Option<u16> }
    if let Ok(s) = std::fs::read_to_string(config_path()) {
        if let Ok(c) = serde_json::from_str::<Cfg>(&s) {
            return c.port.unwrap_or(7891);
        }
    }
    // Cria config padrão
    let _ = std::fs::write(config_path(), r#"{"port":7891}"#);
    7891
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("remote_link_relay=info,info")
        .init();

    let port = load_port();
    let addr = format!("0.0.0.0:{}", port);

    let listener = TcpListener::bind(&addr).await
        .unwrap_or_else(|e| { eprintln!("Não foi possível bindar {}: {}", addr, e); std::process::exit(1); });

    println!("╔══════════════════════════════════════╗");
    println!("║     RemoteLink Relay v0.1.0          ║");
    println!("╠══════════════════════════════════════╣");
    println!("║  Porta   : {}                       ║", port);
    println!("║  Config  : ~/.remote-link-relay.json ║");
    println!("╚══════════════════════════════════════╝");
    println!("Aguardando peers...");

    let peers: PeerMap = Arc::new(Mutex::new(HashMap::new()));

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("Conexão de {}", addr);
                let peers = peers.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, peers).await {
                        warn!("Peer {}: {}", addr, e);
                    }
                });
            }
            Err(e) => warn!("Erro accept: {}", e),
        }
    }
}

async fn handle(mut stream: TcpStream, peers: PeerMap) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let peer_addr = stream.peer_addr()?;

    let (reader, mut writer) = stream.into_split();
    let mut buf = BufReader::new(reader);
    let mut line = String::new();
    buf.read_line(&mut line).await?;

    let msg: PeerMsg = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("JSON inválido: {}", e))?;

    match msg.action.as_str() {
        "register" => {
            let id = msg.id.unwrap_or_else(|| Uuid::new_v4().to_string());
            info!("Registrado: {} ({})", id, peer_addr);

            let resp = serde_json::to_string(&PeerResp { ok: true, id: Some(id.clone()), reason: None })? + "\n";
            writer.write_all(resp.as_bytes()).await?;

            let stream = buf.into_inner().reunite(writer)?;
            peers.lock().await.insert(id, stream);
        }

        "connect" => {
            let id = match msg.id {
                Some(i) if !i.is_empty() => i,
                _ => {
                    let resp = serde_json::to_string(&PeerResp { ok: false, id: None, reason: Some("ID não informado".into()) })? + "\n";
                    writer.write_all(resp.as_bytes()).await?;
                    return Ok(());
                }
            };

            let other = peers.lock().await.remove(&id);
            match other {
                None => {
                    warn!("ID não encontrado: {}", id);
                    let resp = serde_json::to_string(&PeerResp { ok: false, id: None, reason: Some(format!("ID '{}' não encontrado", id)) })? + "\n";
                    writer.write_all(resp.as_bytes()).await?;
                }
                Some(mut other_stream) => {
                    info!("Emparelhando peers via ID: {}", id);

                    // Notifica quem pediu connect
                    let resp = serde_json::to_string(&PeerResp { ok: true, id: None, reason: None })? + "\n";
                    writer.write_all(resp.as_bytes()).await?;

                    // Notifica quem estava esperando
                    let notif = serde_json::to_string(&PeerResp { ok: true, id: Some("peer_connected".into()), reason: None })? + "\n";
                    other_stream.write_all(notif.as_bytes()).await?;

                    // Ponte bidirecional
                    let mut stream_b = buf.into_inner().reunite(writer)?;
                    bridge(&mut other_stream, &mut stream_b).await;
                    info!("Sessão encerrada: {}", id);
                }
            }
        }

        _ => { warn!("Ação desconhecida: {}", msg.action); }
    }
    Ok(())
}

async fn bridge(a: &mut TcpStream, b: &mut TcpStream) {
    let (mut ra, mut wa) = a.split();
    let (mut rb, mut wb) = b.split();
    tokio::select! {
        _ = tokio::io::copy(&mut ra, &mut wb) => {}
        _ = tokio::io::copy(&mut rb, &mut wa) => {}
    }
}
