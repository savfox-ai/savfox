use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub enum ScreenshotFormat {
    Jpeg,
    #[default]
    Png,
    Webp,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub scale: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ScreenshotOptions {
    pub format: ScreenshotFormat,
    pub quality: Option<u8>,
    pub clip: Option<Viewport>,
    pub from_surface: bool,
    pub capture_beyond_viewport: bool,
    pub optimize_for_speed: bool,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            format: ScreenshotFormat::Png,
            quality: None,
            clip: None,
            from_surface: true,
            capture_beyond_viewport: false,
            optimize_for_speed: false,
        }
    }
}

impl ScreenshotOptions {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use] 
    pub fn format(mut self, format: ScreenshotFormat) -> Self {
        self.format = format;
        self
    }

    #[must_use] 
    pub fn quality(mut self, quality: u8) -> Self {
        self.quality = Some(quality);
        self
    }

    #[must_use] 
    pub fn clip(mut self, x: f64, y: f64, width: f64, height: f64) -> Self {
        self.clip = Some(Viewport {
            x,
            y,
            width,
            height,
            scale: None,
        });
        self
    }

    #[must_use] 
    pub fn full_page(mut self, width: f64, height: f64) -> Self {
        self.clip = Some(Viewport {
            x: 0.0,
            y: 0.0,
            width,
            height,
            scale: Some(1.0),
        });
        self.capture_beyond_viewport = true;
        self
    }
}
