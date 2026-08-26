use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
};

use fontdue::{Font, FontSettings};
use winit::event_loop::EventLoopProxy;

use crate::event::AppEvent;

pub const ATLAS_SIZE: u32 = 1024;
pub const DEFAULT_FONT_PATH: &str =
    "/usr/share/fonts/jetbrains-mono-fonts/JetBrainsMono-Regular.otf";
pub const DEFAULT_FONT_SIZE: f32 = 18.0;
const GLYPH_PADDING: u32 = 1;
const FALLBACK_REQUEST_CAPACITY: usize = 64;
const FALLBACK_RESPONSE_CAPACITY: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct GlyphInfo {
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub width: f32,
    pub height: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub advance_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl AtlasRegion {
    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

pub struct GlyphAtlas {
    pub pixels: Vec<u8>,
    pub cell_width: f32,
    pub cell_height: f32,
    pub solid_uv_min: [f32; 2],
    pub solid_uv_max: [f32; 2],
    glyphs: HashMap<char, GlyphInfo>,
    missing: HashSet<char>,
    fonts: Vec<Arc<Font>>,
    loaded_font_paths: HashSet<PathBuf>,
    fallback_requests: SyncSender<char>,
    fallback_responses: Receiver<FallbackResponse>,
    pending_fallbacks: HashSet<char>,
    font_size: f32,
    baseline: f32,
    packer: ShelfPacker,
    dirty_region: Option<AtlasRegion>,
}

struct FallbackResponse {
    character: char,
    fonts: Vec<(PathBuf, Arc<Font>)>,
    found: bool,
    elapsed: std::time::Duration,
}

#[derive(Debug)]
pub enum FontError {
    Read {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
    Parse(&'static str),
    MissingLineMetrics,
    AtlasFull,
    FallbackThread(std::io::Error),
}

impl fmt::Display for FontError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => {
                write!(formatter, "could not read {}: {error}", path.display())
            }
            Self::Parse(error) => write!(formatter, "could not parse configured font: {error}"),
            Self::MissingLineMetrics => formatter.write_str("font has no horizontal line metrics"),
            Self::AtlasFull => formatter.write_str("glyphs exceeded the fixed atlas"),
            Self::FallbackThread(error) => {
                write!(formatter, "could not start fallback font loader: {error}")
            }
        }
    }
}

impl Error for FontError {}

impl GlyphAtlas {
    pub fn load(
        path: &Path,
        fallback_paths: &[PathBuf],
        logical_font_size: f32,
        scale_factor: f64,
        event_proxy: Option<EventLoopProxy<AppEvent>>,
    ) -> Result<Self, FontError> {
        let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor.clamp(0.5, 4.0) as f32
        } else {
            1.0
        };
        let font_size = logical_font_size * scale_factor;
        let font = load_font(path)?;
        let line_metrics = font
            .horizontal_line_metrics(font_size)
            .ok_or(FontError::MissingLineMetrics)?;
        let (reference_metrics, _) = font.rasterize('M', font_size);
        let cell_width = reference_metrics.advance_width.ceil().max(1.0);
        let cell_height = line_metrics.new_line_size.ceil().max(1.0);
        let baseline = line_metrics.ascent.ceil();

        let fonts = vec![Arc::new(font)];
        let loaded_font_paths = HashSet::from([path.to_path_buf()]);
        let mut configured_fallbacks = VecDeque::new();
        let mut seen = loaded_font_paths.clone();
        for fallback_path in fallback_paths {
            if seen.insert(fallback_path.clone()) {
                configured_fallbacks.push_back(fallback_path.clone());
            }
        }
        let configured_fallback_count = configured_fallbacks.len();
        let (fallback_requests, fallback_request_receiver) =
            mpsc::sync_channel(FALLBACK_REQUEST_CAPACITY);
        let (fallback_response_sender, fallback_responses) =
            mpsc::sync_channel(FALLBACK_RESPONSE_CAPACITY);
        let loader_paths = loaded_font_paths.clone();
        thread::Builder::new()
            .name("flash-font-fallback".to_owned())
            .spawn(move || {
                fallback_loader(
                    fallback_request_receiver,
                    fallback_response_sender,
                    configured_fallbacks,
                    loader_paths,
                    event_proxy,
                )
            })
            .map_err(FontError::FallbackThread)?;

        let mut pixels = vec![0; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        pixels[0] = u8::MAX;
        let mut atlas = Self {
            pixels,
            glyphs: HashMap::new(),
            missing: HashSet::new(),
            fonts,
            loaded_font_paths,
            fallback_requests,
            fallback_responses,
            pending_fallbacks: HashSet::new(),
            font_size,
            baseline,
            packer: ShelfPacker::new(ATLAS_SIZE, ATLAS_SIZE, 2, 1),
            dirty_region: None,
            cell_width,
            cell_height,
            solid_uv_min: [0.5 / ATLAS_SIZE as f32; 2],
            solid_uv_max: [0.5 / ATLAS_SIZE as f32; 2],
        };

        for byte in b' '..=b'~' {
            atlas.cache_glyph(char::from(byte))?;
        }
        atlas.dirty_region = None;

        tracing::info!(
            path = %path.display(),
            font_size,
            cell_width,
            cell_height,
            configured_fallback_count,
            "initialized lazy Unicode font atlas"
        );

        Ok(atlas)
    }

    #[cfg(test)]
    pub fn load_default(scale_factor: f64) -> Result<Self, FontError> {
        Self::load(
            Path::new(DEFAULT_FONT_PATH),
            &[],
            DEFAULT_FONT_SIZE,
            scale_factor,
            None,
        )
    }

    pub fn glyph(&mut self, character: char) -> Option<GlyphInfo> {
        if let Some(glyph) = self.glyphs.get(&character) {
            return Some(*glyph);
        }
        if self.missing.contains(&character) {
            return self.replacement_glyph();
        }
        match self.cache_glyph(character) {
            Ok(Some(glyph)) => Some(glyph),
            Ok(None) => {
                if !self.pending_fallbacks.contains(&character) {
                    match self.fallback_requests.try_send(character) {
                        Ok(()) => {
                            self.pending_fallbacks.insert(character);
                        }
                        Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => {
                            self.missing.insert(character);
                        }
                    }
                }
                self.replacement_glyph()
            }
            Err(FontError::AtlasFull) => {
                tracing::warn!(character = %character, "glyph atlas is full; using replacement glyph");
                self.missing.insert(character);
                self.replacement_glyph()
            }
            Err(error) => {
                tracing::warn!(%error, character = %character, "could not cache glyph");
                self.missing.insert(character);
                self.replacement_glyph()
            }
        }
    }

    pub fn take_dirty_region(&mut self) -> Option<AtlasRegion> {
        self.dirty_region.take()
    }

    pub fn drain_fallbacks(&mut self) -> usize {
        let mut loaded_count = 0;
        while let Ok(response) = self.fallback_responses.try_recv() {
            self.pending_fallbacks.remove(&response.character);
            for (path, font) in response.fonts {
                if self.loaded_font_paths.insert(path) {
                    self.fonts.push(font);
                    loaded_count += 1;
                }
            }
            if !response.found {
                self.missing.insert(response.character);
            }
            tracing::debug!(
                character = %response.character,
                found = response.found,
                loaded_font_count = loaded_count,
                load_us = response.elapsed.as_micros(),
                "font fallback response received"
            );
        }
        loaded_count
    }

    #[cfg(test)]
    fn has_pending_fallbacks(&self) -> bool {
        !self.pending_fallbacks.is_empty()
    }

    fn replacement_glyph(&self) -> Option<GlyphInfo> {
        self.glyphs
            .get(&'\u{fffd}')
            .or_else(|| self.glyphs.get(&'?'))
            .copied()
    }

    fn cache_glyph(&mut self, character: char) -> Result<Option<GlyphInfo>, FontError> {
        let face_index = self
            .fonts
            .iter()
            .position(|font| font.lookup_glyph_index(character) != 0);
        let Some(face_index) = face_index else {
            return Ok(None);
        };
        let (metrics, bitmap) = self.fonts[face_index].rasterize(character, self.font_size);
        let glyph = if metrics.width == 0 || metrics.height == 0 {
            GlyphInfo {
                uv_min: [0.0; 2],
                uv_max: [0.0; 2],
                width: 0.0,
                height: 0.0,
                x_offset: 0.0,
                y_offset: 0.0,
                advance_width: metrics.advance_width,
            }
        } else {
            let packed = self
                .packer
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
                self.pixels[destination..destination + metrics.width]
                    .copy_from_slice(&bitmap[source..source + metrics.width]);
            }
            let atlas_size = ATLAS_SIZE as f32;
            let region = AtlasRegion {
                x: glyph_x,
                y: glyph_y,
                width: metrics.width as u32,
                height: metrics.height as u32,
            };
            self.dirty_region = Some(
                self.dirty_region
                    .map_or(region, |dirty| dirty.union(region)),
            );
            GlyphInfo {
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
                y_offset: self.baseline - (metrics.height as f32 + metrics.ymin as f32),
                advance_width: metrics.advance_width,
            }
        };
        self.glyphs.insert(character, glyph);
        Ok(Some(glyph))
    }
}

fn fallback_loader(
    requests: Receiver<char>,
    responses: SyncSender<FallbackResponse>,
    mut configured_fallbacks: VecDeque<PathBuf>,
    mut loaded_paths: HashSet<PathBuf>,
    event_proxy: Option<EventLoopProxy<AppEvent>>,
) {
    let mut fonts: Vec<(PathBuf, Arc<Font>)> = Vec::new();
    while let Ok(character) = requests.recv() {
        let started_at = std::time::Instant::now();
        let mut found = fonts
            .iter()
            .any(|(_, font)| font.lookup_glyph_index(character) != 0);
        let mut new_fonts = Vec::new();

        while !found {
            let Some(path) = configured_fallbacks.pop_front() else {
                break;
            };
            found = load_fallback_candidate(
                path,
                character,
                &mut loaded_paths,
                &mut fonts,
                &mut new_fonts,
                "configured",
            );
        }
        if !found {
            for path in system_fallback_paths(character) {
                if load_fallback_candidate(
                    path,
                    character,
                    &mut loaded_paths,
                    &mut fonts,
                    &mut new_fonts,
                    "system",
                ) {
                    found = true;
                    break;
                }
            }
        }

        if responses
            .send(FallbackResponse {
                character,
                fonts: new_fonts,
                found,
                elapsed: started_at.elapsed(),
            })
            .is_err()
        {
            return;
        }
        if let Some(event_proxy) = event_proxy.as_ref()
            && event_proxy.send_event(AppEvent::FontFallbackReady).is_err()
        {
            return;
        }
    }
}

fn load_fallback_candidate(
    path: PathBuf,
    character: char,
    loaded_paths: &mut HashSet<PathBuf>,
    fonts: &mut Vec<(PathBuf, Arc<Font>)>,
    new_fonts: &mut Vec<(PathBuf, Arc<Font>)>,
    source: &'static str,
) -> bool {
    if !loaded_paths.insert(path.clone()) {
        return false;
    }
    let started_at = std::time::Instant::now();
    match load_font(&path) {
        Ok(font) => {
            let font = Arc::new(font);
            let contains_character = font.lookup_glyph_index(character) != 0;
            fonts.push((path.clone(), Arc::clone(&font)));
            new_fonts.push((path.clone(), font));
            tracing::debug!(
                path = %path.display(),
                character = %character,
                source,
                load_us = started_at.elapsed().as_micros(),
                contains_character,
                "loaded fallback font"
            );
            contains_character
        }
        Err(error) => {
            tracing::debug!(
                %error,
                path = %path.display(),
                source,
                load_us = started_at.elapsed().as_micros(),
                "skipping unusable fallback font"
            );
            false
        }
    }
}

fn load_font(path: &Path) -> Result<Font, FontError> {
    let bytes = fs::read(path).map_err(|error| FontError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    Font::from_bytes(
        bytes,
        FontSettings {
            load_substitutions: false,
            ..FontSettings::default()
        },
    )
    .map_err(FontError::Parse)
}

fn system_fallback_paths(character: char) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let pattern = format!(":charset={:x}", u32::from(character));
    if let Ok(output) = Command::new("fc-match")
        .args(["-f", "%{file}\\n", &pattern])
        .output()
        && output.status.success()
    {
        paths.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.is_empty())
                .map(PathBuf::from),
        );
    }
    paths.extend([
        PathBuf::from("/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf"),
        PathBuf::from("/usr/share/fonts/google-droid-sans-fonts/DroidSansFallbackFull.ttf"),
        PathBuf::from("/usr/share/fonts/google-noto-emoji-fonts/NotoEmoji-Regular.ttf"),
        PathBuf::from("/usr/share/fonts/gdouros-symbola/Symbola.ttf"),
    ]);
    paths.retain(|path| path.is_file());
    paths
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

    fn wait_for_fallbacks(atlas: &mut GlyphAtlas) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while atlas.has_pending_fallbacks() && std::time::Instant::now() < deadline {
            atlas.drain_fallbacks();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        atlas.drain_fallbacks();
        assert!(!atlas.has_pending_fallbacks(), "fallback loader timed out");
    }

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
        let mut atlas = GlyphAtlas::load_default(1.0).expect("JetBrains Mono must be installed");
        assert_eq!(atlas.pixels.len(), (ATLAS_SIZE * ATLAS_SIZE) as usize);
        assert!(atlas.glyph('A').is_some());
        assert!(atlas.glyph('~').is_some());
        assert!(
            atlas.take_dirty_region().is_none(),
            "preloaded atlas starts clean"
        );
        assert!(atlas.glyph('é').is_some());
    }

    #[test]
    fn scale_factor_changes_physical_cell_metrics() {
        let normal = GlyphAtlas::load_default(1.0).expect("JetBrains Mono must be installed");
        let scaled = GlyphAtlas::load_default(2.0).expect("JetBrains Mono must be installed");
        assert!(scaled.cell_width > normal.cell_width);
        assert!(scaled.cell_height > normal.cell_height);
    }

    #[test]
    fn selects_an_explicit_fallback_for_a_missing_primary_glyph() {
        let fallback = std::path::PathBuf::from(
            "/usr/share/fonts/google-droid-sans-fonts/DroidSansFallbackFull.ttf",
        );
        if !fallback.is_file() {
            return;
        }
        let mut atlas = GlyphAtlas::load(
            std::path::Path::new(super::DEFAULT_FONT_PATH),
            &[fallback],
            super::DEFAULT_FONT_SIZE,
            1.0,
            None,
        )
        .expect("test fonts must load");
        let _ = atlas.glyph('界');
        wait_for_fallbacks(&mut atlas);
        let glyph = atlas
            .glyph('界')
            .expect("fallback should contain CJK glyphs");
        assert!(glyph.width > 0.0);
        assert!(!atlas.missing.contains(&'界'));
        assert!(
            atlas.take_dirty_region().is_some(),
            "lazy Unicode glyph dirties the atlas"
        );
        assert!(atlas.fonts.len() > 1);
    }

    #[test]
    fn rasterizes_monochrome_emoji_fallbacks() {
        let fallback = std::path::PathBuf::from(
            "/usr/share/fonts/google-noto-emoji-fonts/NotoEmoji-Regular.ttf",
        );
        if !fallback.is_file() {
            return;
        }
        let mut atlas = GlyphAtlas::load(
            std::path::Path::new(super::DEFAULT_FONT_PATH),
            &[fallback],
            super::DEFAULT_FONT_SIZE,
            1.0,
            None,
        )
        .expect("test fonts must load");
        let _ = atlas.glyph('🙂');
        wait_for_fallbacks(&mut atlas);
        let glyph = atlas
            .glyph('🙂')
            .expect("emoji fallback should contain glyph");
        assert!(glyph.width > 0.0 && glyph.height > 0.0);
    }

    #[test]
    fn measures_first_use_latency_for_mixed_unicode_sample() {
        let mut atlas = GlyphAtlas::load_default(1.0).expect("JetBrains Mono must be installed");
        let sample = "café naïve résumé ✓ ✗ → ← ↑ ↓ ★ ♥ λ π ∞ 日本語 中文 한국어 e\u{301} a\u{308} n\u{303} 😀 🚀 ❤️ 👍🏽";
        let total_started_at = std::time::Instant::now();
        for character in sample.chars() {
            let started_at = std::time::Instant::now();
            let _ = atlas.glyph(character);
            let elapsed = started_at.elapsed();
            if elapsed >= std::time::Duration::from_millis(1) {
                eprintln!(
                    "first glyph {character:?}: {:.3} ms",
                    elapsed.as_secs_f64() * 1_000.0
                );
            }
        }
        eprintln!(
            "mixed Unicode render-path first-use total: {:.3} ms",
            total_started_at.elapsed().as_secs_f64() * 1_000.0
        );
        let background_started_at = std::time::Instant::now();
        wait_for_fallbacks(&mut atlas);
        eprintln!(
            "mixed Unicode background fallback total: {:.3} ms",
            background_started_at.elapsed().as_secs_f64() * 1_000.0
        );
        for character in sample.chars() {
            let _ = atlas.glyph(character);
        }
    }
}
