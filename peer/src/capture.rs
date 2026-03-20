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
        let monitor = monitors
            .into_iter()
            .find(|m| m.is_primary())
            .or_else(|| Monitor::all().ok()?.into_iter().next())
            .ok_or_else(|| anyhow::anyhow!("Nenhum monitor encontrado"))?;
        let width  = monitor.width();
        let height = monitor.height();
        Ok(Self { monitor, width, height })
    }

    pub fn capture_jpeg(&mut self, quality: u8) -> Result<Vec<u8>> {
        let frame = self.monitor.capture_image()?;
        let (w, h) = (frame.width(), frame.height());
        let (tw, th) = if w > 1920 {
            let r = 1920.0 / w as f64;
            (1920u32, (h as f64 * r) as u32)
        } else {
            (w, h)
        };

        // Converte RGBA → RGB direto sem from_fn pixel a pixel
        let rgb: ImageBuffer<Rgb<u8>, Vec<u8>> = if tw != w || th != h {
            let resized = image::imageops::resize(&frame, tw, th, image::imageops::FilterType::Nearest);
            let raw: Vec<u8> = resized.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
            ImageBuffer::from_raw(tw, th, raw).ok_or_else(|| anyhow::anyhow!("buffer inválido"))?
        } else {
            let raw: Vec<u8> = frame.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
            ImageBuffer::from_raw(w, h, raw).ok_or_else(|| anyhow::anyhow!("buffer inválido"))?
        };

        let mut buf = std::io::Cursor::new(Vec::new());
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        enc.encode_image(&rgb)?;
        Ok(buf.into_inner())
    }

    pub fn size(&self) -> (u32, u32) { (self.width, self.height) }
}
