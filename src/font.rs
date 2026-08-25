use std::{error::Error, fmt, fs, path::Path};

use fontdue::{Font, FontSettings};

pub const ATLAS_SIZE: u32 = 1024;
const FONT_PATH: &str = "/usr/share/fonts/jetbrains-mono-fonts/JetBrainsMono-Regular.otf";
const FONT_SIZE_PX: f32 = 18.0;
const GLYPH_PADDING: u32 = 1;

#[derive(Debug, Clone, Copy)]
pub struct GlyphInfo {
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub width: f32,
    pub height: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

pub struct GlyphAtlas {
    pub pixels: Vec<u8>,
    pub glyphs: [Option<GlyphInfo>; 128],
    pub cell_width: f32,
    pub cell_height: f32,
    pub solid_uv_min: [f32; 2],
    pub solid_uv_max: [f32; 2],
}

#[derive(Debug)]
pub enum FontError {
    Read(std::io::Error),
    Parse(&'static str),
    MissingLineMetrics,
    AtlasFull,
}

impl fmt::Display for FontError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "could not read {FONT_PATH}: {error}"),
            Self::Parse(error) => write!(formatter, "could not parse JetBrains Mono: {error}"),
            Self::MissingLineMetrics => formatter.write_str("font has no horizontal line metrics"),
            Self::AtlasFull => {
                formatter.write_str("printable ASCII glyphs exceeded the fixed atlas")
            }
        }
    }
}

impl Error for FontError {}

impl GlyphAtlas {
    pub fn load_default() -> Result<Self, FontError> {
        let bytes = fs::read(Path::new(FONT_PATH)).map_err(FontError::Read)?;
        let font = Font::from_bytes(bytes, FontSettings::default()).map_err(FontError::Parse)?;
        let line_metrics = font
            .horizontal_line_metrics(FONT_SIZE_PX)
            .ok_or(FontError::MissingLineMetrics)?;
        let (reference_metrics, _) = font.rasterize('M', FONT_SIZE_PX);
        let cell_width = reference_metrics.advance_width.ceil().max(1.0);
        let cell_height = line_metrics.new_line_size.ceil().max(1.0);
        let baseline = line_metrics.ascent.ceil();

        let mut pixels = vec![0; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        pixels[0] = u8::MAX;
        let mut glyphs = [None; 128];
        let mut packer = ShelfPacker::new(ATLAS_SIZE, ATLAS_SIZE, 2, 1);

        for byte in b' '..=b'~' {
            let character = char::from(byte);
            let (metrics, bitmap) = font.rasterize(character, FONT_SIZE_PX);
            if metrics.width == 0 || metrics.height == 0 {
                glyphs[usize::from(byte)] = Some(GlyphInfo {
                    uv_min: [0.0; 2],
                    uv_max: [0.0; 2],
                    width: 0.0,
                    height: 0.0,
                    x_offset: 0.0,
                    y_offset: 0.0,
                });
                continue;
            }

            let packed = packer
                .place(
                    metrics.width as u32 + GLYPH_PADDING * 2,
                    metrics.height as u32 + GLYPH_PADDING * 2,
                )
                .ok_or(FontError::AtlasFull)?;
            let glyph_x = packed.0 + GLYPH_PADDING;
            let glyph_y = packed.1 + GLYPH_PADDING;

            for row in 0..metrics.height {
                let source = row * metrics.width;
                let destination = (glyph_y as usize + row) * ATLAS_SIZE as usize + glyph_x as usize;
                pixels[destination..destination + metrics.width]
                    .copy_from_slice(&bitmap[source..source + metrics.width]);
            }

            let atlas_size = ATLAS_SIZE as f32;
            glyphs[usize::from(byte)] = Some(GlyphInfo {
                uv_min: [
                    (glyph_x as f32 + 0.5) / atlas_size,
                    (glyph_y as f32 + 0.5) / atlas_size,
                ],
                uv_max: [
                    (glyph_x as f32 + metrics.width as f32 - 0.5) / atlas_size,
                    (glyph_y as f32 + metrics.height as f32 - 0.5) / atlas_size,
                ],
                width: metrics.width as f32,
                height: metrics.height as f32,
                x_offset: metrics.xmin as f32,
                y_offset: baseline - (metrics.height as f32 + metrics.ymin as f32),
            });
        }

        tracing::info!(
            path = FONT_PATH,
            font_size = FONT_SIZE_PX,
            cell_width,
            cell_height,
            "rasterized printable ASCII font atlas"
        );

        Ok(Self {
            pixels,
            glyphs,
            cell_width,
            cell_height,
            solid_uv_min: [0.5 / ATLAS_SIZE as f32; 2],
            solid_uv_max: [0.5 / ATLAS_SIZE as f32; 2],
        })
    }

    pub fn glyph(&self, character: char) -> Option<GlyphInfo> {
        let index = character as usize;
        (index < self.glyphs.len())
            .then(|| self.glyphs[index])
            .flatten()
    }
}

struct ShelfPacker {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    shelf_height: u32,
}

impl ShelfPacker {
    fn new(width: u32, height: u32, x: u32, y: u32) -> Self {
        Self {
            width,
            height,
            x,
            y,
            shelf_height: 0,
        }
    }

    fn place(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if width > self.width || height > self.height {
            return None;
        }
        if self.x + width > self.width {
            self.x = 0;
            self.y += self.shelf_height;
            self.shelf_height = 0;
        }
        if self.y + height > self.height {
            return None;
        }

        let position = (self.x, self.y);
        self.x += width;
        self.shelf_height = self.shelf_height.max(height);
        Some(position)
    }
}

#[cfg(test)]
mod tests {
    use super::{ATLAS_SIZE, GlyphAtlas, ShelfPacker};

    #[test]
    fn shelf_packer_wraps_rows_and_rejects_overflow() {
        let mut packer = ShelfPacker::new(8, 8, 0, 0);
        assert_eq!(packer.place(5, 3), Some((0, 0)));
        assert_eq!(packer.place(4, 2), Some((0, 3)));
        assert_eq!(packer.place(8, 3), Some((0, 5)));
        assert_eq!(packer.place(1, 1), None);
    }

    #[test]
    fn default_atlas_contains_printable_ascii() {
        let atlas = GlyphAtlas::load_default().expect("JetBrains Mono must be installed");
        assert_eq!(atlas.pixels.len(), (ATLAS_SIZE * ATLAS_SIZE) as usize);
        assert!(atlas.glyph('A').is_some());
        assert!(atlas.glyph('~').is_some());
        assert!(atlas.glyph('é').is_none());
    }
}
