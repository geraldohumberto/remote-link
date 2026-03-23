use anyhow::Result;
use image::{ImageBuffer, Rgba, RgbImage};
use xcap::Monitor;
use crate::protocol::{BlockInfo, BLOCK_SIZE, MonitorInfo};

pub struct Capturer {
    monitor:    Monitor,
    pub width:  u32,
    pub height: u32,
    prev_frame: Option<RgbImage>,
}

impl Capturer {
    pub fn new() -> Result<Self> {
        Self::new_with_index(0)
    }

    pub fn new_with_index(index: usize) -> Result<Self> {
        let mut monitors = Monitor::all()?;
        // Primário primeiro, depois os demais por posição X
        monitors.sort_by_key(|m| (!m.is_primary(), m.x()));
        let monitor = monitors.into_iter().nth(index)
            .or_else(|| Monitor::all().ok()?.into_iter().find(|m| m.is_primary()))
            .or_else(|| Monitor::all().ok()?.into_iter().next())
            .ok_or_else(|| anyhow::anyhow!("Monitor {} nao encontrado", index))?;
        let width  = monitor.width();
        let height = monitor.height();
        Ok(Self { monitor, width, height, prev_frame: None })
    }

    pub fn list_monitors() -> Vec<MonitorInfo> {
        let mut monitors = Monitor::all().unwrap_or_default();
        // Primário primeiro, depois os demais por posição X
        monitors.sort_by_key(|m| (!m.is_primary(), m.x()));
        monitors
            .into_iter()
            .enumerate()
            .map(|(i, m)| MonitorInfo {
                index:    i as u8,
                width:    m.width(),
                height:   m.height(),
                primary:  m.is_primary(),
                name:     m.name().to_string(),
                offset_x: m.x(),
                offset_y: m.y(),
            })
            .collect()
    }

    pub fn switch_monitor(&mut self, index: usize) -> Result<()> {
        let mut monitors = Monitor::all()?;
        monitors.sort_by_key(|m| (!m.is_primary(), m.x()));
        if let Some(m) = monitors.into_iter().nth(index) {
            self.width  = m.width();
            self.height = m.height();
            self.monitor = m;
            self.prev_frame = None;
        }
        Ok(())
    }

    pub fn capture_delta(&mut self, quality: u8) -> Result<Option<(u32, u32, Vec<(BlockInfo, Vec<u8>)>)>> {
        let frame = self.monitor.capture_image()?;
        let (w, h) = (frame.width(), frame.height());
        let (tw, th) = if w > 1920 {
            let r = 1920.0 / w as f64;
            (1920u32, (h as f64 * r) as u32)
        } else {
            (w, h)
        };
        let current = to_rgb(&frame, tw, th);
        let mut changed_blocks: Vec<(BlockInfo, Vec<u8>)> = Vec::new();
        if let Some(prev) = &self.prev_frame {
            let cols = (tw + BLOCK_SIZE - 1) / BLOCK_SIZE;
            let rows = (th + BLOCK_SIZE - 1) / BLOCK_SIZE;

            for row in 0..rows {
                for col in 0..cols {
                    let bx = col * BLOCK_SIZE;
                    let by = row * BLOCK_SIZE;
                    let bw = BLOCK_SIZE.min(tw - bx);
                    let bh = BLOCK_SIZE.min(th - by);
                    if block_changed(prev, &current, bx, by, bw, bh) {
                        let block_img = crop_block(&current, bx, by, bw, bh);
                        let jpeg = jpeg_encode(&block_img, quality)?;
                        let size = jpeg.len() as u32;
                        changed_blocks.push((BlockInfo { x: bx, y: by, w: bw, h: bh, size }, jpeg));
                    }
                }
            }
        } else {
            // Primeiro frame — envia em blocos pequenos igual ao delta
            // Evita travar o canal com um frame gigante na conexão inicial
            let cols = (tw + BLOCK_SIZE - 1) / BLOCK_SIZE;
            let rows = (th + BLOCK_SIZE - 1) / BLOCK_SIZE;
            for row in 0..rows {
                for col in 0..cols {
                    let bx = col * BLOCK_SIZE;
                    let by = row * BLOCK_SIZE;
                    let bw = BLOCK_SIZE.min(tw - bx);
                    let bh = BLOCK_SIZE.min(th - by);
                    let block_img = crop_block(&current, bx, by, bw, bh);
                    let jpeg = jpeg_encode(&block_img, quality)?;
                    let size = jpeg.len() as u32;
                    changed_blocks.push((BlockInfo { x: bx, y: by, w: bw, h: bh, size }, jpeg));
                }
            }
        }
        self.prev_frame = Some(current);
        if changed_blocks.is_empty() { Ok(None) } else { Ok(Some((tw, th, changed_blocks))) }
    }

    pub fn capture_jpeg(&mut self, quality: u8) -> Result<Vec<u8>> {
        let frame = self.monitor.capture_image()?;
        let (w, h) = (frame.width(), frame.height());
        let (tw, th) = if w > 1920 { let r = 1920.0/w as f64; (1920u32,(h as f64*r) as u32) } else { (w,h) };
        let rgb = to_rgb(&frame, tw, th);
        self.prev_frame = Some(rgb.clone());
        jpeg_encode(&rgb, quality)
    }

    pub fn size(&self) -> (u32, u32) { (self.width, self.height) }
}

fn to_rgb(frame: &ImageBuffer<Rgba<u8>, Vec<u8>>, tw: u32, th: u32) -> RgbImage {
    let (w, h) = (frame.width(), frame.height());
    if tw != w || th != h {
        let resized = image::imageops::resize(frame, tw, th, image::imageops::FilterType::Nearest);
        let raw: Vec<u8> = resized.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
        ImageBuffer::from_raw(tw, th, raw).unwrap()
    } else {
        let raw: Vec<u8> = frame.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
        ImageBuffer::from_raw(w, h, raw).unwrap()
    }
}

fn block_changed(prev: &RgbImage, curr: &RgbImage, bx: u32, by: u32, bw: u32, bh: u32) -> bool {
    for y in by..by+bh {
        for x in bx..bx+bw {
            let p = prev.get_pixel(x, y);
            let c = curr.get_pixel(x, y);
            if p[0].abs_diff(c[0]) > 8 || p[1].abs_diff(c[1]) > 8 || p[2].abs_diff(c[2]) > 8 {
                return true;
            }
        }
    }
    false
}

fn crop_block(img: &RgbImage, bx: u32, by: u32, bw: u32, bh: u32) -> RgbImage {
    let mut out = RgbImage::new(bw, bh);
    for y in 0..bh { for x in 0..bw { out.put_pixel(x, y, *img.get_pixel(bx+x, by+y)); } }
    out
}

fn jpeg_encode(img: &RgbImage, quality: u8) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode_image(img)?;
    Ok(buf.into_inner())
}
