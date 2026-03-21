use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use std::sync::Arc;
use tokio::sync::Mutex;

pub const DEFAULT_PEER_PORT:  u16  = 7890;
pub const DEFAULT_RELAY_PORT: u16  = 7891;
pub const DEFAULT_PASSWORD:   &str = "remotelink123";
pub const CHUNK_SIZE:         usize = 65_536;
pub const JPEG_QUALITY:       u8   = 55;
pub const FPS_TARGET:         u64  = 15;
pub const BLOCK_SIZE:         u32  = 64;

pub type Writer = Arc<Mutex<OwnedWriteHalf>>;
pub type Reader = OwnedReadHalf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index:   u8,
    pub width:   u32,
    pub height:  u32,
    pub primary: bool,
    pub name:    String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Message {
    Auth        { password: String, monitor_index: Option<u8> },
    AuthOk      { screen_w: u32, screen_h: u32, platform: String, peer_id: String },
    AuthFail    { reason: String },
    FrameInfo   { width: u32, height: u32, size: u32 },
    // Delta: envia só os blocos que mudaram
    FrameDelta  { screen_w: u32, screen_h: u32, monitor_id: u8, blocks: Vec<BlockInfo> },
    // Monitores
    MonitorList    { monitors: Vec<MonitorInfo> },
    SwitchMonitor  { index: u8 },
    Input(InputEvent),
    Clipboard   { text: String },
    FileListReq { folder: Option<String> },
    FileListRes { folder: String, items: Vec<FileItem> },
    FileUpload  { filename: String, filesize: u64 },
    FileDownload{ filename: String, path: String },
    FileChunk   { size: u32 },
    FileDone    { filename: String, bytes: u64 },
    FileError   { reason: String },
    Ping, Pong, Disconnect,
}

/// Metadados de um bloco que mudou
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    pub x:    u32,   // coluna do bloco em pixels
    pub y:    u32,   // linha do bloco em pixels
    pub w:    u32,   // largura real (pode ser menor na borda direita/inferior)
    pub h:    u32,
    pub size: u32,   // tamanho do JPEG deste bloco em bytes
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InputEvent {
    MouseMove { x: i32, y: i32 },
    MouseDown { x: i32, y: i32, button: MouseBtn },
    MouseUp   { x: i32, y: i32, button: MouseBtn },
    MouseDbl  { x: i32, y: i32 },
    Scroll    { x: i32, y: i32, dy: i32 },
    KeyDown   { key: String },
    KeyUp     { key: String },
    TypeText  { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MouseBtn { Left, Middle, Right }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileItem {
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub path: String,
}

pub async fn send_msg(w: &Writer, msg: &Message) -> anyhow::Result<()> {
    let json = serde_json::to_vec(msg)?;
    let len  = (json.len() as u32).to_be_bytes();
    let mut g = w.lock().await;
    g.write_all(&len).await?;
    g.write_all(&json).await?;
    Ok(())
}

pub async fn send_msg_bytes(w: &Writer, msg: &Message, data: &[u8]) -> anyhow::Result<()> {
    let json = serde_json::to_vec(msg)?;
    let len  = (json.len() as u32).to_be_bytes();
    let mut g = w.lock().await;
    g.write_all(&len).await?;
    g.write_all(&json).await?;
    g.write_all(data).await?;
    Ok(())
}

pub async fn recv_msg(r: &mut Reader) -> anyhow::Result<Message> {
    let mut lb = [0u8; 4];
    r.read_exact(&mut lb).await?;
    let len = u32::from_be_bytes(lb) as usize;
    anyhow::ensure!(len < 8 * 1024 * 1024, "Mensagem muito grande");
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let msg: Message = serde_json::from_slice(&buf)?;
    Ok(msg)
}

pub async fn recv_bytes(r: &mut Reader, n: usize) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}
