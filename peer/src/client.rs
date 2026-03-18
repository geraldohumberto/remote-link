use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    relay: Option<(String, u16)>,   // Some((relay_host, relay_port))
    mut cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    // Tenta conexão direta primeiro, depois via relay se configurado
    let stream = match try_connect(&host, port, &relay, &evt_tx).await {
        Some(s) => s,
        None    => return,
    };

    stream.set_nodelay(true).ok();
    let (mut reader, writer_raw) = stream.into_split();
    let writer: Writer = Arc::new(Mutex::new(writer_raw));

    // Auth
    if send_msg(&writer, &Message::Auth { password }).await.is_err() { return; }

    match recv_msg(&mut reader).await {
        Ok(Message::AuthOk { screen_w, screen_h, platform, .. }) => {
            info!("Autenticado: {}x{} {}", screen_w, screen_h, platform);
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

    // Recv task
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

    // Command loop
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
                        let chunk_data: Vec<u8> = chunk.to_vec();
                        let _ = send_msg_bytes(&writer, &Message::FileChunk { size: chunk_data.len() as u32 }, &chunk_data).await;
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
    recv_task.abort();
}

/// Tenta conexão direta, e se falhar e relay estiver configurado, tenta via relay
async fn try_connect(
    host: &str,
    port: u16,
    relay: &Option<(String, u16)>,
    evt_tx: &mpsc::Sender<Evt>,
) -> Option<TcpStream> {
    let direct_addr = format!("{}:{}", host, port);

    // Tenta direto primeiro (timeout 5s)
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(&direct_addr),
    ).await {
        Ok(Ok(stream)) => {
            info!("Conectado diretamente em {}", direct_addr);
            return Some(stream);
        }
        _ => {
            info!("Conexao direta falhou em {}", direct_addr);
        }
    }

    // Se tem relay configurado, tenta via relay
    if let Some((relay_host, relay_port)) = relay {
        let relay_addr = format!("{}:{}", relay_host, relay_port);
        info!("Tentando via relay: {}", relay_addr);

        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            connect_via_relay(&relay_addr, host, port),
        ).await {
            Ok(Ok(stream)) => {
                info!("Conectado via relay");
                return Some(stream);
            }
            Ok(Err(e)) => {
                let _ = evt_tx.send(Evt::Error {
                    reason: format!("Conexao direta e relay falharam. Relay: {}", e),
                }).await;
            }
            Err(_) => {
                let _ = evt_tx.send(Evt::Error {
                    reason: "Timeout ao conectar via relay.".into(),
                }).await;
            }
        }
    } else {
        let _ = evt_tx.send(Evt::Error {
            reason: format!("Nao foi possivel conectar em {}. Verifique o IP e a porta, ou configure um relay.", direct_addr),
        }).await;
    }

    None
}

/// Conecta ao relay e pede pra ser emparelhado com o peer destino
async fn connect_via_relay(relay_addr: &str, target_host: &str, target_port: u16) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(relay_addr).await?;
    stream.set_nodelay(true)?;

    // Envia pedido de conexão com ID = "host:porta" do destino
    let peer_id = format!("{}:{}", target_host, target_port);
    let msg = serde_json::json!({"action": "connect", "id": peer_id}).to_string() + "\n";
    stream.write_all(msg.as_bytes()).await?;

    // Lê resposta do relay
    let (reader, writer) = stream.into_split();
    let mut buf = BufReader::new(reader);
    let mut line = String::new();
    buf.read_line(&mut line).await?;

    let resp: serde_json::Value = serde_json::from_str(line.trim())?;
    if resp["ok"].as_bool() != Some(true) {
        anyhow::bail!("{}", resp["reason"].as_str().unwrap_or("Relay recusou conexao"));
    }

    // Reconstrói o stream
    Ok(buf.into_inner().reunite(writer)?)
}
