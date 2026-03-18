use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};
use tracing::{info, warn};
use uuid::Uuid;

use crate::capture::Capturer;
use crate::config::Config;
use crate::input::Injector;
use crate::protocol::*;

pub async fn run(config: Arc<Config>) {
    let addr = format!("0.0.0.0:{}", config.port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l)  => { info!("Servidor escutando em {}", addr); l }
        Err(e) => { warn!("Nao foi possivel bindar {}: {}", addr, e); return; }
    };
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("Conexao de {}", peer);
                let cfg = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, cfg).await {
                        warn!("Sessao encerrada: {}", e);
                    }
                });
            }
            Err(e) => { warn!("Erro accept: {}", e); tokio::time::sleep(Duration::from_secs(1)).await; }
        }
    }
}

async fn handle(stream: TcpStream, config: Arc<Config>) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, writer_raw) = stream.into_split();
    let writer: Writer = Arc::new(Mutex::new(writer_raw));

    match recv_msg(&mut reader).await? {
        Message::Auth { password } if password == config.password => {
            let cap = Capturer::new()?;
            let (sw, sh) = cap.size();
            send_msg(&writer, &Message::AuthOk {
                screen_w: sw, screen_h: sh,
                platform: std::env::consts::OS.to_string(),
                peer_id:  Uuid::new_v4().to_string(),
            }).await?;
        }
        Message::Auth { .. } => {
            send_msg(&writer, &Message::AuthFail { reason: "Senha incorreta".into() }).await?;
            anyhow::bail!("Senha errada");
        }
        _ => anyhow::bail!("Protocolo inesperado na auth"),
    }

    let w2   = writer.clone();
    let cfg2 = config.clone();
    let cap  = Arc::new(Mutex::new(Capturer::new()?));
    let cap2 = cap.clone();
    let frame_task = tokio::spawn(async move {
        let mut tick = interval(Duration::from_millis(1000 / cfg2.fps));
        loop {
            tick.tick().await;
            let result = {
                let mut c = cap2.lock().await;
                c.capture_jpeg(cfg2.jpeg_quality)
            };
            if let Ok(jpeg) = result {
                let (w, h) = { cap2.lock().await.size() };
                let size = jpeg.len() as u32;
                if send_msg_bytes(&w2, &Message::FrameInfo { width: w, height: h, size }, &jpeg).await.is_err() {
                    break;
                }
            }
        }
    });

    let mut inj = Injector::new()?;
    loop {
        match recv_msg(&mut reader).await? {
            Message::Input(ev) => { let _ = inj.inject(&ev); }
            Message::Clipboard { text } => {
                if let Ok(mut c) = arboard::Clipboard::new() { let _ = c.set_text(text); }
            }
            Message::FileListReq { folder } => {
                let base = folder.map(PathBuf::from).unwrap_or_else(|| config.shared_path());
                send_msg(&writer, &file_list(&base)).await?;
            }
            Message::FileDownload { filename, .. } => {
                let path = config.shared_path().join(&filename);
                do_download(&writer, &path, &filename).await?;
            }
            Message::FileUpload { filename, filesize } => {
                let dest = config.shared_path().join(
                    PathBuf::from(&filename).file_name().unwrap_or_default()
                );
                do_upload(&writer, &mut reader, &dest, &filename, filesize).await?;
            }
            Message::Ping => { send_msg(&writer, &Message::Pong).await?; }
            Message::Disconnect => break,
            _ => {}
        }
    }
    frame_task.abort();
    Ok(())
}

fn file_list(folder: &PathBuf) -> Message {
    let mut items = Vec::new();
    if let Ok(rd) = std::fs::read_dir(folder) {
        for e in rd.flatten() {
            let meta   = e.metadata();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size   = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            items.push(FileItem {
                name: e.file_name().to_string_lossy().to_string(),
                kind: if is_dir { "dir" } else { "file" }.into(),
                size,
                path: e.path().to_string_lossy().to_string(),
            });
        }
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Message::FileListRes { folder: folder.to_string_lossy().to_string(), items }
}

async fn do_download(w: &Writer, path: &PathBuf, filename: &str) -> anyhow::Result<()> {
    if !path.is_file() {
        return send_msg(w, &Message::FileError { reason: format!("Nao encontrado: {}", filename) }).await;
    }
    let data  = tokio::fs::read(path).await?;
    let total = data.len() as u64;
    for chunk in data.chunks(CHUNK_SIZE) {
        send_msg_bytes(w, &Message::FileChunk { size: chunk.len() as u32 }, chunk).await?;
    }
    send_msg(w, &Message::FileDone { filename: filename.to_string(), bytes: total }).await
}

async fn do_upload(
    w: &Writer, r: &mut Reader,
    dest: &PathBuf, filename: &str, filesize: u64,
) -> anyhow::Result<()> {
    let mut data = Vec::with_capacity(filesize as usize);
    let mut received = 0u64;
    loop {
        match recv_msg(r).await? {
            Message::FileChunk { size } => {
                let chunk: Vec<u8> = recv_bytes(r, size as usize).await?;
                data.extend_from_slice(&chunk);
                received += size as u64;
            }
            Message::FileDone { .. } => break,
            _ => {}
        }
    }
    tokio::fs::write(dest, &data).await?;
    send_msg(w, &Message::FileDone { filename: filename.to_string(), bytes: received }).await
}
