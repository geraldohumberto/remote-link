use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    id:     Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

fn load_port() -> u16 {
    #[derive(Deserialize)]
    struct Cfg { port: Option<u16> }
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".remote-link-relay.json");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(c) = serde_json::from_str::<Cfg>(&s) {
            return c.port.unwrap_or(7891);
        }
    }
    let _ = std::fs::write(&path, r#"{"port":7891}"#);
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
        .unwrap_or_else(|e| { eprintln!("Nao foi possivel bindar {}: {}", addr, e); std::process::exit(1); });

    println!("RemoteLink Relay v0.1.0");
    println!("Porta: {} | Config: ~/.remote-link-relay.json", port);
    println!("Aguardando peers...");

    let peers: PeerMap = Arc::new(Mutex::new(HashMap::new()));

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("Conexao de {}", addr);
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
    let mut buf  = BufReader::new(reader);
    let mut line = String::new();
    buf.read_line(&mut line).await?;

    let msg: PeerMsg = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("JSON invalido: {}", e))?;

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
                    let resp = serde_json::to_string(&PeerResp { ok: false, id: None, reason: Some("ID nao informado".into()) })? + "\n";
                    writer.write_all(resp.as_bytes()).await?;
                    return Ok(());
                }
            };

            let other = peers.lock().await.remove(&id);
            match other {
                None => {
                    warn!("ID nao encontrado: {}", id);
                    let resp = serde_json::to_string(&PeerResp { ok: false, id: None, reason: Some(format!("ID '{}' nao encontrado", id)) })? + "\n";
                    writer.write_all(resp.as_bytes()).await?;
                }
                Some(mut other_stream) => {
                    info!("Emparelhando peers via ID: {}", id);

                    let resp = serde_json::to_string(&PeerResp { ok: true, id: None, reason: None })? + "\n";
                    writer.write_all(resp.as_bytes()).await?;

                    let notif = serde_json::to_string(&PeerResp { ok: true, id: Some("peer_connected".into()), reason: None })? + "\n";
                    other_stream.write_all(notif.as_bytes()).await?;

                    let mut stream_b = buf.into_inner().reunite(writer)?;
                    bridge(&mut other_stream, &mut stream_b).await;
                    info!("Sessao encerrada: {}", id);
                }
            }
        }

        _ => { warn!("Acao desconhecida: {}", msg.action); }
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
