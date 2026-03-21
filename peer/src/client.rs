use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    SwitchMonitor  { index: u8 },
    Disconnect,
}

#[derive(Debug, Clone)]
pub enum Evt {
    Connected   { screen_w: u32, screen_h: u32, platform: String },
    Frame       { jpeg: Vec<u8> },
    FrameDelta  { monitor_id: u8, screen_w: u32, screen_h: u32, blocks: Vec<(crate::protocol::BlockInfo, Vec<u8>)> },
    MonitorList { monitors: Vec<crate::protocol::MonitorInfo> },
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
    relay_id: String,
    monitor_index: Option<u8>,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    // Detecta relay WebSocket
    let is_ws = relay.as_ref().map(|(h, _)| {
        let h = h.trim().to_lowercase();
        h.starts_with("https://") || h.starts_with("http://") ||
        h.starts_with("wss://")   || h.starts_with("ws://")   ||
        h.contains(".workers.dev") || h.contains(".pages.dev")
    }).unwrap_or(false);

    if is_ws {
        let relay_host = relay.unwrap().0;
        connect_ws(relay_host, host, port, password, cmd_rx, evt_tx).await;
        return;
    }

    // Se tem relay e relay_id configurado, vai direto pro relay sem tentar IP local
    if relay.is_some() && !relay_id.is_empty() {
        if let Some((rhost, rport)) = relay {
            match tcp_relay(&rhost, rport, &relay_id).await {
                Ok(stream) => {
                    stream.set_nodelay(true).ok();
                    tcp_session(stream, password, monitor_index, cmd_rx, evt_tx).await;
                }
                Err(e) => {
                    let _ = evt_tx.send(Evt::Error {
                        reason: format!("Relay TCP falhou: {}", e),
                    }).await;
                }
            }
        }
        return;
    }

    // Tenta TCP direto
    let direct_addr = format!("{}:{}", host, port);
    let stream = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(&direct_addr),
    ).await {
        Ok(Ok(s)) => { info!("Direto: {}", direct_addr); s }
        _ => {
            if let Some((rhost, rport)) = relay {
                let id = if relay_id.is_empty() {
                    format!("{}:{}", host, port)
                } else {
                    relay_id.clone()
                };
                match tcp_relay(&rhost, rport, &id).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = evt_tx.send(Evt::Error {
                            reason: format!("Falhou direto e relay TCP: {}", e),
                        }).await;
                        return;
                    }
                }
            } else {
                let _ = evt_tx.send(Evt::Error {
                    reason: format!("Nao conectou em {}. Configure o relay para conexoes pela internet.", direct_addr),
                }).await;
                return;
            }
        }
    };

    stream.set_nodelay(true).ok();
    tcp_session(stream, password, monitor_index, cmd_rx, evt_tx).await;
}

// ── Sessão TCP direta ────────────────────────────────────────────────────
async fn tcp_session(
    stream: TcpStream,
    password: String,
    monitor_index: Option<u8>,
    cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    let (mut reader, writer_raw) = stream.into_split();
    let writer: Writer = Arc::new(Mutex::new(writer_raw));

    if send_msg(&writer, &Message::Auth { password, monitor_index }).await.is_err() { return; }

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
                Ok(Message::MonitorList { monitors }) => {
                    let _ = tx2.send(Evt::MonitorList { monitors }).await;
                }
                Ok(Message::FrameDelta { monitor_id, screen_w, screen_h, blocks }) => {
                    let mut block_data = Vec::with_capacity(blocks.len());
                    let mut ok = true;
                    for b in &blocks {
                        match recv_bytes(&mut reader, b.size as usize).await {
                            Ok(jpeg) => block_data.push((b.clone(), jpeg)),
                            Err(_)   => { ok = false; break; }
                        }
                    }
                    if ok {
                        let _ = tx2.send(Evt::FrameDelta { monitor_id, screen_w, screen_h, blocks: block_data }).await;
                    }
                }
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
async fn connect_ws(
    relay_host: String,
    target_host: String,
    target_port: u16,
    password: String,
    cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    let base = relay_host.trim().trim_end_matches('/')
        .replace("https://", "wss://")
        .replace("http://", "ws://");

    // ID do peer destino = host:porta
    let peer_id = format!("{}:{}", target_host, target_port);
    let encoded_id = peer_id.replace(":", "%3A").replace(".", "%2E");
    let url = format!("{}/?action=connect&id={}", base, encoded_id);

    info!("Conectando via WS relay: {}", url);

    // Tenta conectar ao relay com retry
    let ws = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        connect_async(&url),
    ).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => {
            let _ = evt_tx.send(Evt::Error {
                reason: format!("Relay WebSocket falhou: {}", e),
            }).await;
            return;
        }
        Err(_) => {
            let _ = evt_tx.send(Evt::Error { reason: "Timeout ao conectar no relay.".into() }).await;
            return;
        }
    };

    info!("WS conectado ao relay");
    let (mut ws_write, mut ws_read) = ws.split();

    // Lê confirmação do relay
    match ws_read.next().await {
        Some(Ok(WsMsg::Text(t))) => {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
            if v["ok"].as_bool() != Some(true) {
                let r = v["reason"].as_str().unwrap_or("Relay recusou").to_string();
                let _ = evt_tx.send(Evt::Error { reason: format!("Relay: {} — O servidor remoto precisa estar rodando e registrado.", r) }).await;
                return;
            }
            info!("Relay confirmado: {}", t);
        }
        Some(Ok(WsMsg::Close(_))) => {
            let _ = evt_tx.send(Evt::Error {
                reason: "Relay fechou conexao. Verifique se o servidor remoto esta rodando.".into()
            }).await;
            return;
        }
        _ => {
            let _ = evt_tx.send(Evt::Error { reason: "Sem resposta do relay.".into() }).await;
            return;
        }
    }

    // Canal de saída: app → WS
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(128);
    // Canal de entrada: WS → app (buffer acumulador)
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(512);

    // Task: WS receber → canal interno
    let in_tx2 = in_tx.clone();
    let ws_recv_task = tokio::spawn(async move {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(WsMsg::Binary(b)) => { if in_tx2.send(b).await.is_err() { break; } }
                Ok(WsMsg::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    // Task: canal interno → WS enviar
    let ws_send_task = tokio::spawn(async move {
        while let Some(data) = out_rx.recv().await {
            if ws_write.send(WsMsg::Binary(data)).await.is_err() { break; }
        }
    });

    // Autenticação — envia via WS
    let auth_bytes = proto_encode(&Message::Auth { password });
    if out_tx.send(auth_bytes).await.is_err() {
        let _ = evt_tx.send(Evt::Error { reason: "Falha ao enviar auth".into() }).await;
        return;
    }

    // Lê resposta de auth
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        proto_decode_from_channel(&mut in_rx),
    ).await {
        Ok(Ok(Message::AuthOk { screen_w, screen_h, platform, .. })) => {
            let _ = evt_tx.send(Evt::Connected { screen_w, screen_h, platform }).await;
        }
        Ok(Ok(Message::AuthFail { reason })) => {
            let _ = evt_tx.send(Evt::Error { reason: format!("Senha errada: {}", reason) }).await;
            ws_recv_task.abort(); ws_send_task.abort();
            return;
        }
        _ => {
            let _ = evt_tx.send(Evt::Error { reason: "Auth via relay falhou.".into() }).await;
            ws_recv_task.abort(); ws_send_task.abort();
            return;
        }
    }

    // Recv task — frames e eventos do relay
    let tx2 = evt_tx.clone();
    let recv_task = tokio::spawn(async move {
        loop {
            match proto_decode_from_channel(&mut in_rx).await {
                Ok(Message::FrameInfo { size, .. }) => {
                    // Lê bytes do frame do próximo chunk
                    match read_raw_from_channel(&mut in_rx, size as usize).await {
                        Ok(jpeg) => { let _ = tx2.send(Evt::Frame { jpeg }).await; }
                        Err(_)   => break,
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

    // Cmd loop — envia comandos via WS
    ws_cmd_loop(cmd_rx, out_tx).await;
    recv_task.abort();
    ws_recv_task.abort();
    ws_send_task.abort();
}

async fn ws_cmd_loop(mut cmd_rx: mpsc::Receiver<Cmd>, out_tx: mpsc::Sender<Vec<u8>>) {
    loop {
        match cmd_rx.recv().await {
            Some(Cmd::Input(ev))       => { let _ = out_tx.send(proto_encode(&Message::Input(ev))).await; }
            Some(Cmd::Clipboard(text)) => { let _ = out_tx.send(proto_encode(&Message::Clipboard { text })).await; }
            Some(Cmd::FileList)        => { let _ = out_tx.send(proto_encode(&Message::FileListReq { folder: None })).await; }
            Some(Cmd::FileDownload { filename, path }) => {
                let _ = out_tx.send(proto_encode(&Message::FileDownload { filename, path })).await;
            }
            Some(Cmd::FileUpload { src }) => {
                if let Ok(data) = tokio::fs::read(&src).await {
                    let filename = std::path::Path::new(&src)
                        .file_name().unwrap_or_default()
                        .to_string_lossy().to_string();
                    let filesize = data.len() as u64;
                    let _ = out_tx.send(proto_encode(&Message::FileUpload { filename: filename.clone(), filesize })).await;
                    for chunk in data.chunks(CHUNK_SIZE) {
                        let mut msg = proto_encode(&Message::FileChunk { size: chunk.len() as u32 });
                        msg.extend_from_slice(chunk);
                        let _ = out_tx.send(msg).await;
                    }
                    let _ = out_tx.send(proto_encode(&Message::FileDone { filename, bytes: filesize })).await;
                }
            }
            Some(Cmd::SwitchMonitor { index }) => {
                let _ = out_tx.send(proto_encode(&Message::SwitchMonitor { index })).await;
            }
            Some(Cmd::Disconnect) | None => {
                let _ = out_tx.send(proto_encode(&Message::Disconnect)).await;
                break;
            }
        }
    }
}

// ── Relay TCP simples ────────────────────────────────────────────────────
async fn tcp_relay(
    relay_host: &str, relay_port: u16,
    peer_id: &str,
) -> anyhow::Result<TcpStream> {
    let relay_addr = format!("{}:{}", relay_host, relay_port);
    let mut stream = TcpStream::connect(&relay_addr).await?;
    stream.set_nodelay(true)?;
    let msg = serde_json::json!({"action":"connect","id": peer_id}).to_string() + "\n";
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

// ── Helpers de protocolo ─────────────────────────────────────────────────
fn proto_encode(msg: &Message) -> Vec<u8> {
    let json = serde_json::to_vec(msg).unwrap_or_default();
    let len  = (json.len() as u32).to_be_bytes();
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&len);
    out.extend_from_slice(&json);
    out
}

async fn proto_decode_from_channel(rx: &mut mpsc::Receiver<Vec<u8>>) -> anyhow::Result<Message> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        if buf.len() >= 4 {
            let len = u32::from_be_bytes([buf[0],buf[1],buf[2],buf[3]]) as usize;
            if buf.len() >= 4 + len {
                let msg: Message = serde_json::from_slice(&buf[4..4+len])?;
                buf.drain(..4+len);
                return Ok(msg);
            }
        }
        let chunk = rx.recv().await.ok_or_else(|| anyhow::anyhow!("canal fechado"))?;
        buf.extend_from_slice(&chunk);
    }
}

async fn read_raw_from_channel(rx: &mut mpsc::Receiver<Vec<u8>>, n: usize) -> anyhow::Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        if buf.len() >= n {
            let out = buf[..n].to_vec();
            buf.drain(..n);
            return Ok(out);
        }
        let chunk = rx.recv().await.ok_or_else(|| anyhow::anyhow!("canal fechado"))?;
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
            Some(Cmd::SwitchMonitor { index }) => {
                let _ = send_msg(&writer, &Message::SwitchMonitor { index }).await;
            }
            Some(Cmd::Disconnect) | None => {
                let _ = send_msg(&writer, &Message::Disconnect).await;
                break;
            }
        }
    }
}