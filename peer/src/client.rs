use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, AsyncReadExt};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMsg};
use futures_util::{SinkExt, StreamExt};
use tracing::info;
use crate::protocol::*;

#[derive(Debug)]
pub enum Cmd {
    Input(InputEvent),
    Clipboard(String),
    FileList,
    FileDownload { filename: String, path: String },
    FileUpload   { src: String },
    Disconnect,
}

#[derive(Debug, Clone)]
pub enum Evt {
    Connected   { screen_w: u32, screen_h: u32, platform: String },
    Frame       { jpeg: Vec<u8> },
    FileList    { folder: String, items: Vec<FileItem> },
    FileDone    { filename: String, bytes: u64 },
    FileError   { reason: String },
    Clipboard   { text: String },
    Error       { reason: String },
    Disconnected,
}

pub async fn connect(
    host: String,
    port: u16,
    password: String,
    relay: Option<(String, u16)>,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    // Detecta relay WebSocket (Cloudflare Workers)
    let is_ws_relay = relay.as_ref().map(|(h, _)| {
        let h = h.trim().to_lowercase();
        h.starts_with("https://") || h.starts_with("http://") ||
        h.starts_with("wss://")   || h.starts_with("ws://")   ||
        h.contains(".workers.dev")
    }).unwrap_or(false);

    if is_ws_relay {
        let relay_host = relay.unwrap().0;
        connect_ws(relay_host, host, port, password, cmd_rx, evt_tx).await;
        return;
    }

    // Tenta conexão TCP direta
    let direct_addr = format!("{}:{}", host, port);
    let stream = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(&direct_addr),
    ).await {
        Ok(Ok(s)) => { info!("Direto: {}", direct_addr); s }
        _ => {
            // Tenta relay TCP
            if let Some((rhost, rport)) = relay {
                match tcp_relay(&rhost, rport, &host, port).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = evt_tx.send(Evt::Error {
                            reason: format!("Falhou direto e relay: {}", e),
                        }).await;
                        return;
                    }
                }
            } else {
                let _ = evt_tx.send(Evt::Error {
                    reason: format!("Nao conectou em {}. Configure relay para internet.", direct_addr),
                }).await;
                return;
            }
        }
    };

    stream.set_nodelay(true).ok();
    tcp_session(stream, password, cmd_rx, evt_tx).await;
}

// ── Sessão TCP ───────────────────────────────────────────────────────────
async fn tcp_session(
    stream: TcpStream,
    password: String,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    let (mut reader, writer_raw) = stream.into_split();
    let writer: Writer = Arc::new(Mutex::new(writer_raw));

    if send_msg(&writer, &Message::Auth { password }).await.is_err() { return; }

    match recv_msg(&mut reader).await {
        Ok(Message::AuthOk { screen_w, screen_h, platform, .. }) => {
            let _ = evt_tx.send(Evt::Connected { screen_w, screen_h, platform }).await;
        }
        Ok(Message::AuthFail { reason }) => {
            let _ = evt_tx.send(Evt::Error { reason: format!("Senha errada: {}", reason) }).await;
            return;
        }
        _ => {
            let _ = evt_tx.send(Evt::Error { reason: "Protocolo inesperado".into() }).await;
            return;
        }
    }

    let tx2 = evt_tx.clone();
    let recv_task = tokio::spawn(async move {
        loop {
            match recv_msg(&mut reader).await {
                Ok(Message::FrameInfo { size, .. }) => {
                    if let Ok(jpeg) = recv_bytes(&mut reader, size as usize).await {
                        let _ = tx2.send(Evt::Frame { jpeg }).await;
                    }
                }
                Ok(Message::FileListRes { folder, items }) => {
                    let _ = tx2.send(Evt::FileList { folder, items }).await;
                }
                Ok(Message::FileChunk { size }) => {
                    let _ = recv_bytes(&mut reader, size as usize).await;
                }
                Ok(Message::FileDone { filename, bytes }) => {
                    let _ = tx2.send(Evt::FileDone { filename, bytes }).await;
                }
                Ok(Message::FileError { reason }) => {
                    let _ = tx2.send(Evt::FileError { reason }).await;
                }
                Ok(Message::Clipboard { text }) => {
                    let _ = tx2.send(Evt::Clipboard { text }).await;
                }
                Ok(Message::Disconnect) | Err(_) => break,
                _ => {}
            }
        }
        let _ = tx2.send(Evt::Disconnected).await;
    });

    cmd_loop(cmd_rx, writer).await;
    recv_task.abort();
}

// ── Relay WebSocket (Cloudflare Workers) ─────────────────────────────────
// Protocolo: cada mensagem RemoteLink vira um WsMsg::Binary
// O Worker faz a ponte binária entre os dois peers
async fn connect_ws(
    relay_host: String,
    target_host: String,
    target_port: u16,
    password: String,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    let base = relay_host.trim().trim_end_matches('/')
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let peer_id = format!("{}:{}", target_host, target_port);
    let url = format!("{}/?action=connect&id={}", base, urlencoding::encode(&peer_id));

    info!("WS relay: {}", url);

    let ws = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        connect_async(&url),
    ).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => {
            let _ = evt_tx.send(Evt::Error { reason: format!("WS relay falhou: {}", e) }).await;
            return;
        }
        Err(_) => {
            let _ = evt_tx.send(Evt::Error { reason: "Timeout no relay.".into() }).await;
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = ws.split();

    // Lê confirmação do relay
    match ws_rx.next().await {
        Some(Ok(WsMsg::Text(t))) => {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
            if v["ok"].as_bool() != Some(true) {
                let r = v["reason"].as_str().unwrap_or("Relay recusou").to_string();
                let _ = evt_tx.send(Evt::Error { reason: format!("Relay: {}", r) }).await;
                return;
            }
            info!("Relay OK: {}", t);
        }
        _ => {
            let _ = evt_tx.send(Evt::Error { reason: "Sem resposta do relay.".into() }).await;
            return;
        }
    }

    // Canal de bytes de saída (app → relay)
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(128);
    // Canal de bytes de entrada (relay → app)
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(256);

    // Task: recebe do WS e manda pro canal interno
    let in_tx2 = in_tx.clone();
    let ws_recv = tokio::spawn(async move {
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(WsMsg::Binary(b)) => { if in_tx2.send(b).await.is_err() { break; } }
                Ok(WsMsg::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    // Task: pega do canal interno e manda pro WS
    let ws_send = tokio::spawn(async move {
        while let Some(data) = out_rx.recv().await {
            if ws_tx.send(WsMsg::Binary(data)).await.is_err() { break; }
        }
    });

    // Autenticação — serializa a mensagem e manda pelo canal
    let auth_bytes = build_msg_bytes(&Message::Auth { password });
    if out_tx.send(auth_bytes).await.is_err() { return; }

    // Lê resposta de auth do canal
    let auth_resp = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        recv_from_channel(&mut in_rx),
    ).await;

    match auth_resp {
        Ok(Ok(Message::AuthOk { screen_w, screen_h, platform, .. })) => {
            let _ = evt_tx.send(Evt::Connected { screen_w, screen_h, platform }).await;
        }
        Ok(Ok(Message::AuthFail { reason })) => {
            let _ = evt_tx.send(Evt::Error { reason: format!("Senha errada: {}", reason) }).await;
            ws_recv.abort(); ws_send.abort();
            return;
        }
        _ => {
            let _ = evt_tx.send(Evt::Error { reason: "Auth via relay falhou.".into() }).await;
            ws_recv.abort(); ws_send.abort();
            return;
        }
    }

    // Recv task — lê frames e eventos do relay
    let tx2 = evt_tx.clone();
    let recv_task = tokio::spawn(async move {
        loop {
            match recv_from_channel(&mut in_rx).await {
                Ok(Message::FrameInfo { size, .. }) => {
                    // Frame: próxima mensagem é o JPEG
                    if let Ok(msg2) = recv_from_channel(&mut in_rx).await {
                        // Frame bytes chegam como Raw
                        if let Message::RawBytes(jpeg) = msg2 {
                            let _ = tx2.send(Evt::Frame { jpeg }).await;
                        }
                    }
                }
                Ok(Message::FileListRes { folder, items }) => {
                    let _ = tx2.send(Evt::FileList { folder, items }).await;
                }
                Ok(Message::FileDone { filename, bytes }) => {
                    let _ = tx2.send(Evt::FileDone { filename, bytes }).await;
                }
                Ok(Message::FileError { reason }) => {
                    let _ = tx2.send(Evt::FileError { reason }).await;
                }
                Ok(Message::Clipboard { text }) => {
                    let _ = tx2.send(Evt::Clipboard { text }).await;
                }
                Ok(Message::Disconnect) | Err(_) => break,
                _ => {}
            }
        }
        let _ = tx2.send(Evt::Disconnected).await;
    });

    // Cmd loop — envia comandos pelo relay
    loop {
        match cmd_rx.recv().await {
            Some(Cmd::Input(ev)) => {
                let _ = out_tx.send(build_msg_bytes(&Message::Input(ev))).await;
            }
            Some(Cmd::Clipboard(text)) => {
                let _ = out_tx.send(build_msg_bytes(&Message::Clipboard { text })).await;
            }
            Some(Cmd::FileList) => {
                let _ = out_tx.send(build_msg_bytes(&Message::FileListReq { folder: None })).await;
            }
            Some(Cmd::FileDownload { filename, path }) => {
                let _ = out_tx.send(build_msg_bytes(&Message::FileDownload { filename, path })).await;
            }
            Some(Cmd::FileUpload { src }) => {
                if let Ok(data) = tokio::fs::read(&src).await {
                    let filename = std::path::Path::new(&src)
                        .file_name().unwrap_or_default()
                        .to_string_lossy().to_string();
                    let filesize = data.len() as u64;
                    let _ = out_tx.send(build_msg_bytes(&Message::FileUpload { filename: filename.clone(), filesize })).await;
                    for chunk in data.chunks(CHUNK_SIZE) {
                        let _ = out_tx.send(build_msg_bytes_with_data(&Message::FileChunk { size: chunk.len() as u32 }, chunk)).await;
                    }
                    let _ = out_tx.send(build_msg_bytes(&Message::FileDone { filename, bytes: filesize })).await;
                }
            }
            Some(Cmd::Disconnect) | None => {
                let _ = out_tx.send(build_msg_bytes(&Message::Disconnect)).await;
                break;
            }
        }
    }

    recv_task.abort();
    ws_recv.abort();
    ws_send.abort();
}

// ── Relay TCP simples ────────────────────────────────────────────────────
async fn tcp_relay(
    relay_host: &str, relay_port: u16,
    target_host: &str, target_port: u16,
) -> anyhow::Result<TcpStream> {
    let relay_addr = format!("{}:{}", relay_host, relay_port);
    let mut stream = TcpStream::connect(&relay_addr).await?;
    stream.set_nodelay(true)?;
    let peer_id = format!("{}:{}", target_host, target_port);
    let msg = serde_json::json!({"action":"connect","id":peer_id}).to_string() + "\n";
    stream.write_all(msg.as_bytes()).await?;
    let (reader, writer) = stream.into_split();
    let mut buf = BufReader::new(reader);
    let mut line = String::new();
    buf.read_line(&mut line).await?;
    let resp: serde_json::Value = serde_json::from_str(line.trim())?;
    if resp["ok"].as_bool() != Some(true) {
        anyhow::bail!("{}", resp["reason"].as_str().unwrap_or("Relay recusou"));
    }
    Ok(buf.into_inner().reunite(writer)?)
}

// ── Helpers ──────────────────────────────────────────────────────────────
fn build_msg_bytes(msg: &Message) -> Vec<u8> {
    let json = serde_json::to_vec(msg).unwrap_or_default();
    let len = (json.len() as u32).to_be_bytes();
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&len);
    out.extend_from_slice(&json);
    out
}

fn build_msg_bytes_with_data(msg: &Message, data: &[u8]) -> Vec<u8> {
    let json = serde_json::to_vec(msg).unwrap_or_default();
    let len = (json.len() as u32).to_be_bytes();
    let mut out = Vec::with_capacity(4 + json.len() + data.len());
    out.extend_from_slice(&len);
    out.extend_from_slice(&json);
    out.extend_from_slice(data);
    out
}

async fn recv_from_channel(rx: &mut mpsc::Receiver<Vec<u8>>) -> anyhow::Result<Message> {
    // Acumula chunks até ter uma mensagem completa
    let mut buf: Vec<u8> = Vec::new();
    loop {
        if buf.len() >= 4 {
            let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            if buf.len() >= 4 + len {
                let msg: Message = serde_json::from_slice(&buf[4..4+len])?;
                // Remove bytes consumidos
                buf.drain(..4+len);
                return Ok(msg);
            }
        }
        let chunk = rx.recv().await.ok_or_else(|| anyhow::anyhow!("channel fechado"))?;
        buf.extend_from_slice(&chunk);
    }
}

async fn cmd_loop(mut cmd_rx: mpsc::Receiver<Cmd>, writer: Writer) {
    loop {
        match cmd_rx.recv().await {
            Some(Cmd::Input(ev))       => { let _ = send_msg(&writer, &Message::Input(ev)).await; }
            Some(Cmd::Clipboard(text)) => { let _ = send_msg(&writer, &Message::Clipboard { text }).await; }
            Some(Cmd::FileList)        => { let _ = send_msg(&writer, &Message::FileListReq { folder: None }).await; }
            Some(Cmd::FileDownload { filename, path }) => {
                let _ = send_msg(&writer, &Message::FileDownload { filename, path }).await;
            }
            Some(Cmd::FileUpload { src }) => {
                if let Ok(data) = tokio::fs::read(&src).await {
                    let filename = std::path::Path::new(&src)
                        .file_name().unwrap_or_default()
                        .to_string_lossy().to_string();
                    let filesize = data.len() as u64;
                    let _ = send_msg(&writer, &Message::FileUpload { filename: filename.clone(), filesize }).await;
                    for chunk in data.chunks(CHUNK_SIZE) {
                        let d: Vec<u8> = chunk.to_vec();
                        let _ = send_msg_bytes(&writer, &Message::FileChunk { size: d.len() as u32 }, &d).await;
                    }
                    let _ = send_msg(&writer, &Message::FileDone { filename, bytes: filesize }).await;
                }
            }
            Some(Cmd::Disconnect) | None => {
                let _ = send_msg(&writer, &Message::Disconnect).await;
                break;
            }
        }
    }
}
