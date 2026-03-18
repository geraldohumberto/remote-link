// src/capture.rs
use anyhow::Result;
use image::{ImageBuffer, Rgb};
use xcap::Monitor;

pub struct Capturer {
    monitor: Monitor,
    pub width:  u32,
    pub height: u32,
}

impl Capturer {
    pub fn new() -> Result<Self> {
        let monitors = Monitor::all()?;
        let monitor  = monitors.into_iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| Monitor::all().ok()?.into_iter().next())
            .ok_or_else(|| anyhow::anyhow!("Nenhum monitor encontrado"))?;
        let width  = monitor.width()?;
        let height = monitor.height()?;
        Ok(Self { monitor, width, height })
    }

    pub fn capture_jpeg(&mut self, quality: u8) -> Result<Vec<u8>> {
        let frame = self.monitor.capture_image()?;
        let (w, h) = (frame.width(), frame.height());

        // Reduz pra no máximo 1920px de largura
        let (tw, th) = if w > 1920 {
            let r = 1920.0 / w as f64;
            (1920u32, (h as f64 * r) as u32)
        } else {
            (w, h)
        };

        let rgb: ImageBuffer<Rgb<u8>, Vec<u8>> = if tw != w || th != h {
            let resized = image::imageops::resize(&frame, tw, th, image::imageops::FilterType::Nearest);
            ImageBuffer::from_fn(tw, th, |x, y| {
                let p = resized.get_pixel(x, y);
                Rgb([p[0], p[1], p[2]])
            })
        } else {
            ImageBuffer::from_fn(w, h, |x, y| {
                let p = frame.get_pixel(x, y);
                Rgb([p[0], p[1], p[2]])
            })
        };

        let mut buf = std::io::Cursor::new(Vec::new());
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        enc.encode_image(&rgb)?;
        Ok(buf.into_inner())
    }

    pub fn size(&self) -> (u32, u32) { (self.width, self.height) }
}
