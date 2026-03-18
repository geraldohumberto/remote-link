// src/client.rs
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};
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
    FileProgress{ filename: String, bytes: u64, total: u64 },
    FileDone    { filename: String, bytes: u64 },
    FileError   { reason: String },
    Clipboard   { text: String },
    Error       { reason: String },
    Disconnected,
}

pub async fn connect(
    host: String, port: u16, password: String,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    evt_tx: mpsc::Sender<Evt>,
) {
    let addr = format!("{}:{}", host, port);
    let stream = match TcpStream::connect(&addr).await {
        Ok(s)  => s,
        Err(e) => {
            let _ = evt_tx.send(Evt::Error { reason: format!("Não conectou em {}: {}", addr, e) }).await;
            return;
        }
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
        _ => { let _ = evt_tx.send(Evt::Error { reason: "Protocolo inesperado".into() }).await; return; }
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
            Some(Cmd::Input(ev)) => { let _ = send_msg(&writer, &Message::Input(ev)).await; }
            Some(Cmd::Clipboard(text)) => { let _ = send_msg(&writer, &Message::Clipboard { text }).await; }
            Some(Cmd::FileList) => { let _ = send_msg(&writer, &Message::FileListReq { folder: None }).await; }
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
                        let _ = send_msg_bytes(&writer, &Message::FileChunk { size: chunk.len() as u32 }, chunk).await;
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
