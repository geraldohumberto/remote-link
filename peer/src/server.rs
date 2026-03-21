use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

use crate::capture::Capturer;
use crate::config::Config;
use crate::input::Injector;
use crate::protocol::*;

pub async fn run(config: Arc<Config>) {
    // Inicia registro no relay em background (se configurado)
    if let Some((relay_host, relay_port)) = config.relay() {
        let cfg = config.clone();
        tokio::spawn(relay_register_loop(relay_host, relay_port, cfg));
    }

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l)  => { info!("Servidor escutando em {}", addr); l }
        Err(e) => { warn!("Nao foi possivel bindar {}: {}", addr, e); return; }
    };
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("Conexao direta de {}", peer);
                let cfg = config.clone();
                tokio::spawn(handle(stream, cfg));
            }
            Err(e) => {
                warn!("Erro accept: {}", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Loop que mantém o servidor sempre registrado no relay.
/// Quando um peer conecta via relay, entrega o stream ao handle().
/// Após cada sessão (ou falha), re-registra automaticamente.
async fn relay_register_loop(relay_host: String, relay_port: u16, config: Arc<Config>) {
    let my_id = config.machine_id.clone();
    info!("Relay register loop — ID: {}", my_id);

    loop {
        match relay_register_once(&relay_host, relay_port, &my_id, config.clone()).await {
            Ok(()) => {
                info!("Sessao relay encerrada, re-registrando em 3s...");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            Err(e) => {
                warn!("Relay falhou: {} — tentando em 30s", e);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}

async fn relay_register_once(
    relay_host: &str,
    relay_port: u16,
    my_id: &str,
    config: Arc<Config>,
) -> anyhow::Result<()> {
    let relay_addr = format!("{}:{}", relay_host, relay_port);

    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect(&relay_addr),
    ).await
    .map_err(|_| anyhow::anyhow!("Timeout conectando no relay"))?
    .map_err(|e| anyhow::anyhow!("Erro conectando no relay: {}", e))?;

    stream.set_nodelay(true)?;
    let (reader, mut writer) = stream.into_split();
    let mut buf = BufReader::new(reader);

    // Envia registro
    let reg_msg = serde_json::json!({"action": "register", "id": my_id}).to_string() + "\n";
    writer.write_all(reg_msg.as_bytes()).await?;

    // Lê confirmação do registro
    let mut line = String::new();
    buf.read_line(&mut line).await?;
    let resp: serde_json::Value = serde_json::from_str(line.trim())?;
    if resp["ok"].as_bool() != Some(true) {
        anyhow::bail!("Relay recusou: {}", resp["reason"].as_str().unwrap_or("?"));
    }
    info!("Registrado no relay {} com ID '{}'", relay_addr, my_id);

    // Aguarda notificação de peer conectado: {"ok":true,"id":"peer_connected"}
    line.clear();
    buf.read_line(&mut line).await?;
    let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();

    if notif["id"].as_str() == Some("peer_connected") {
        info!("Peer conectou via relay — iniciando sessao");
        // Reconstrói o TcpStream e entrega ao handle()
        // A partir daqui o relay faz bridge de bytes — transparente pro protocolo
        let stream = buf.into_inner().reunite(writer)?;
        handle(stream, config).await?;
    } else {
        warn!("Notificacao inesperada do relay: {}", line.trim());
    }

    Ok(())
}

async fn handle(stream: TcpStream, config: Arc<Config>) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, writer_raw) = stream.into_split();
    let writer: Writer = Arc::new(Mutex::new(writer_raw));

    // Auth
    match recv_msg(&mut reader).await? {
        Message::Auth { password } if password == config.password => {
            let (sw, sh) = tokio::task::spawn_blocking(|| {
                Capturer::new()
                    .map(|c| c.size())
                    .unwrap_or((1920, 1080))
            }).await?;

            send_msg(&writer, &Message::AuthOk {
                screen_w: sw,
                screen_h: sh,
                platform: std::env::consts::OS.to_string(),
                peer_id:  Uuid::new_v4().to_string(),
            }).await?;

            // Envia lista de monitores logo após AuthOk
            let monitors = Capturer::list_monitors();
            send_msg(&writer, &Message::MonitorList { monitors }).await?;
        }
        Message::Auth { .. } => {
            send_msg(&writer, &Message::AuthFail { reason: "Senha incorreta".into() }).await?;
            anyhow::bail!("Senha errada");
        }
        _ => anyhow::bail!("Protocolo inesperado na auth"),
    }

    // Canal: (screen_w, screen_h, blocos_delta)
    type DeltaMsg = (u32, u32, Vec<(crate::protocol::BlockInfo, Vec<u8>)>);
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<DeltaMsg>(2);
    let (switch_tx, switch_rx)   = std::sync::mpsc::channel::<usize>();
    let quality = config.jpeg_quality;
    let fps     = config.fps;

    let capture_thread = std::thread::spawn(move || {
        let mut cap = match Capturer::new() {
            Ok(c)  => c,
            Err(e) => { warn!("Capturer falhou: {}", e); return; }
        };
        let interval_ms = std::time::Duration::from_millis(1000 / fps);
        loop {
            if let Ok(idx) = switch_rx.try_recv() {
                let _ = cap.switch_monitor(idx);
            }
            let t0 = std::time::Instant::now();
            match cap.capture_delta(quality) {
                Ok(Some(delta)) => {
                    if frame_tx.blocking_send(delta).is_err() { break; }
                }
                Ok(None) => {} // nada mudou, pula
                Err(e) => { warn!("Captura falhou: {}", e); }
            }
            let elapsed = t0.elapsed();
            if elapsed < interval_ms {
                std::thread::sleep(interval_ms - elapsed);
            }
        }
    });

    let w2 = writer.clone();
    let frame_task = tokio::spawn(async move {
        while let Some((sw, sh, blocks)) = frame_rx.recv().await {
            let block_infos: Vec<crate::protocol::BlockInfo> = blocks.iter().map(|(b,_)| b.clone()).collect();
            if send_msg(&w2, &Message::FrameDelta {
                screen_w: sw, screen_h: sh, blocks: block_infos,
            }).await.is_err() { break; }
            for (_, jpeg) in &blocks {
                let mut g = w2.lock().await;
                if g.write_all(jpeg).await.is_err() { break; }
            }
        }
    });

    let mut inj = match Injector::new() {
        Ok(i)  => i,
        Err(e) => { warn!("Injector falhou: {}", e); anyhow::bail!("Injector: {}", e); }
    };

    loop {
        match recv_msg(&mut reader).await? {
            Message::Input(ev) => { let _ = inj.inject(&ev); }
            Message::SwitchMonitor { index } => {
                let _ = switch_tx.send(index as usize);
            }
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
    drop(capture_thread);
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
        return send_msg(w, &Message::FileError {
            reason: format!("Nao encontrado: {}", filename),
        }).await;
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
