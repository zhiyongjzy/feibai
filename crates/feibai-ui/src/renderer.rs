use cosmic_text::{
    Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache,
};
use feibai_core::Candidate;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

pub struct RenderedFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // ARGB8888 pre-multiplied, len = width * height * 4
}

#[derive(Clone)]
pub struct RenderConfig {
    pub font_size: f32,
    pub line_height: f32,
    pub bg_color: [u8; 4],        // [A, R, G, B]
    pub fg_color: [u8; 4],
    pub preedit_color: [u8; 4],
    pub index_color: [u8; 4],     // color for "1." "2." etc
    pub border_color: [u8; 4],
    pub separator_color: [u8; 4],
    pub padding_h: u32,
    pub padding_v: u32,
    pub corner_radius: f32,
    pub border_width: f32,
    pub max_candidates: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
    Flat,
    Blue,
    Sakura,
    Ocean,
    Lavender,
    Tangerine,
    Mint,
}

impl Theme {
    pub fn all() -> &'static [Theme] {
        &[
            Theme::Light,
            Theme::Dark,
            Theme::Flat,
            Theme::Blue,
            Theme::Sakura,
            Theme::Ocean,
            Theme::Lavender,
            Theme::Tangerine,
            Theme::Mint,
        ]
    }

    pub fn next(self) -> Theme {
        match self {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Flat,
            Theme::Flat => Theme::Blue,
            Theme::Blue => Theme::Sakura,
            Theme::Sakura => Theme::Ocean,
            Theme::Ocean => Theme::Lavender,
            Theme::Lavender => Theme::Tangerine,
            Theme::Tangerine => Theme::Mint,
            Theme::Mint => Theme::Light,
        }
    }

    pub fn config(self) -> RenderConfig {
        match self {
            Theme::Light => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [255, 255, 255, 255],
                fg_color: [255, 50, 50, 50],
                preedit_color: [255, 100, 100, 100],
                index_color: [255, 66, 133, 244],
                border_color: [255, 210, 210, 210],
                separator_color: [255, 230, 230, 230],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 8.0,
                border_width: 1.0,
                max_candidates: 9,
            },
            Theme::Dark => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [230, 35, 35, 40],
                fg_color: [255, 220, 220, 225],
                preedit_color: [255, 150, 150, 160],
                index_color: [255, 100, 180, 255],
                border_color: [180, 80, 80, 90],
                separator_color: [120, 100, 100, 110],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 8.0,
                border_width: 1.0,
                max_candidates: 9,
            },
            Theme::Flat => RenderConfig {
                font_size: 17.0,
                line_height: 24.0,
                bg_color: [255, 245, 245, 245],
                fg_color: [255, 30, 30, 30],
                preedit_color: [255, 80, 80, 80],
                index_color: [255, 120, 120, 120],
                border_color: [255, 200, 200, 200],
                separator_color: [255, 220, 220, 220],
                padding_h: 10,
                padding_v: 6,
                corner_radius: 4.0,
                border_width: 1.0,
                max_candidates: 9,
            },
            Theme::Blue => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [255, 255, 255, 255],
                fg_color: [255, 40, 40, 40],
                preedit_color: [255, 70, 70, 70],
                index_color: [255, 25, 118, 210],
                border_color: [255, 187, 222, 251],
                separator_color: [255, 227, 242, 253],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 6.0,
                border_width: 1.5,
                max_candidates: 9,
            },
            Theme::Sakura => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [255, 255, 243, 245],
                fg_color: [255, 80, 40, 60],
                preedit_color: [255, 180, 100, 130],
                index_color: [255, 219, 112, 147],
                border_color: [255, 245, 200, 215],
                separator_color: [255, 250, 218, 228],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 8.0,
                border_width: 1.0,
                max_candidates: 9,
            },
            Theme::Ocean => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [255, 235, 250, 252],
                fg_color: [255, 20, 60, 80],
                preedit_color: [255, 80, 140, 160],
                index_color: [255, 0, 150, 180],
                border_color: [255, 178, 230, 240],
                separator_color: [255, 200, 238, 245],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 8.0,
                border_width: 1.0,
                max_candidates: 9,
            },
            Theme::Lavender => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [255, 248, 242, 255],
                fg_color: [255, 55, 30, 80],
                preedit_color: [255, 140, 100, 170],
                index_color: [255, 138, 92, 200],
                border_color: [255, 220, 200, 240],
                separator_color: [255, 232, 218, 248],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 8.0,
                border_width: 1.0,
                max_candidates: 9,
            },
            Theme::Tangerine => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [255, 255, 248, 240],
                fg_color: [255, 70, 40, 20],
                preedit_color: [255, 160, 110, 60],
                index_color: [255, 230, 120, 30],
                border_color: [255, 250, 215, 180],
                separator_color: [255, 252, 230, 200],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 8.0,
                border_width: 1.0,
                max_candidates: 9,
            },
            Theme::Mint => RenderConfig {
                font_size: 18.0,
                line_height: 26.0,
                bg_color: [255, 240, 253, 247],
                fg_color: [255, 25, 60, 45],
                preedit_color: [255, 80, 145, 110],
                index_color: [255, 22, 163, 100],
                border_color: [255, 187, 237, 210],
                separator_color: [255, 210, 245, 225],
                padding_h: 12,
                padding_v: 8,
                corner_radius: 8.0,
                border_width: 1.0,
                max_candidates: 9,
            },
        }
    }
}

impl Default for RenderConfig {
    fn default() -> Self {
        Theme::Light.config()
    }
}

pub struct CandidateRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    config: RenderConfig,
}

impl CandidateRenderer {
    pub fn new(config: RenderConfig) -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            config,
        }
    }

    pub fn set_config(&mut self, config: RenderConfig) {
        self.config = config;
    }

    pub fn config(&self) -> &RenderConfig {
        &self.config
    }

    pub fn render(
        &mut self,
        preedit: &str,
        candidates: &[Candidate],
        _selected: usize,
    ) -> Option<RenderedFrame> {
        if candidates.is_empty() && preedit.is_empty() {
            return None;
        }

        let cfg = &self.config;
        let metrics = Metrics::new(cfg.font_size, cfg.line_height);
        let attrs = Attrs::new();

        // Measure preedit line
        let mut preedit_buf = Buffer::new(&mut self.font_system, metrics);
        preedit_buf.set_size(&mut self.font_system, Some(f32::MAX), None);
        preedit_buf.set_text(&mut self.font_system, preedit, attrs, Shaping::Advanced);
        preedit_buf.shape_until_scroll(&mut self.font_system, false);

        // Measure candidate line width and keep buffers for reuse in drawing
        let spacing = (cfg.font_size * 0.8) as f32;
        let mut cand_width: f32 = 0.0;
        let mut cand_buffers: Vec<(Buffer, f32, Buffer, f32)> = Vec::new();
        for (i, cand) in candidates.iter().take(cfg.max_candidates).enumerate() {
            let idx_text = format!("{}.", i + 1);
            let mut idx_buf = Buffer::new(&mut self.font_system, metrics);
            idx_buf.set_size(&mut self.font_system, Some(f32::MAX), None);
            idx_buf.set_text(&mut self.font_system, &idx_text, attrs, Shaping::Advanced);
            idx_buf.shape_until_scroll(&mut self.font_system, false);
            let idx_w = measure_buffer_width(&idx_buf);
            cand_width += idx_w;

            let mut text_buf = Buffer::new(&mut self.font_system, metrics);
            text_buf.set_size(&mut self.font_system, Some(f32::MAX), None);
            text_buf.set_text(&mut self.font_system, &cand.text, attrs, Shaping::Advanced);
            text_buf.shape_until_scroll(&mut self.font_system, false);
            let text_w = measure_buffer_width(&text_buf);
            cand_width += text_w;

            cand_buffers.push((idx_buf, idx_w, text_buf, text_w));

            if i + 1 < candidates.len().min(cfg.max_candidates) {
                cand_width += spacing;
            }
        }

        // Calculate dimensions
        let preedit_width = measure_buffer_width(&preedit_buf);
        let content_width = preedit_width.max(cand_width);

        let pad_h = cfg.padding_h;
        let pad_v = cfg.padding_v;
        let width = (content_width as u32 + pad_h * 2).max(80);
        let mut height = pad_v * 2;
        let has_preedit = !preedit.is_empty();
        let has_candidates = !candidates.is_empty();
        if has_preedit {
            height += cfg.line_height as u32;
        }
        if has_preedit && has_candidates {
            height += 4; // separator gap
        }
        if has_candidates {
            height += cfg.line_height as u32;
        }

        // Create pixmap
        let mut pixmap = Pixmap::new(width, height)?;

        // Draw background with rounded corners
        let bg = &cfg.bg_color;
        let mut bg_paint = Paint::default();
        bg_paint.set_color_rgba8(bg[1], bg[2], bg[3], bg[0]);
        bg_paint.anti_alias = true;

        let r = cfg.corner_radius;
        let w = width as f32;
        let h = height as f32;
        let rect_path = rounded_rect_path(w, h, r);
        pixmap.fill_path(&rect_path, &bg_paint, FillRule::Winding, Transform::identity(), None);

        // Draw border
        if cfg.border_width > 0.0 {
            let border = &cfg.border_color;
            let mut stroke_paint = Paint::default();
            stroke_paint.set_color_rgba8(border[1], border[2], border[3], border[0]);
            stroke_paint.anti_alias = true;
            let stroke = tiny_skia::Stroke {
                width: cfg.border_width,
                ..Default::default()
            };
            pixmap.stroke_path(&rect_path, &stroke_paint, &stroke, Transform::identity(), None);
        }

        // Draw preedit text
        let mut y_offset = pad_v as i32;
        if has_preedit {
            let color = Color::rgba(
                cfg.preedit_color[1],
                cfg.preedit_color[2],
                cfg.preedit_color[3],
                cfg.preedit_color[0],
            );
            draw_buffer(
                &mut pixmap,
                &mut self.font_system,
                &mut self.swash_cache,
                &preedit_buf,
                pad_h as i32,
                y_offset,
                color,
            );
            y_offset += cfg.line_height as i32;

            // Draw separator line
            if has_candidates {
                let sep_y = y_offset + 2;
                let sep = &cfg.separator_color;
                let mut sep_paint = Paint::default();
                sep_paint.set_color_rgba8(sep[1], sep[2], sep[3], sep[0]);
                let sep_path = {
                    let mut pb = PathBuilder::new();
                    pb.move_to(pad_h as f32, sep_y as f32);
                    pb.line_to(w - pad_h as f32, sep_y as f32);
                    pb.finish().unwrap()
                };
                let stroke = tiny_skia::Stroke { width: 1.0, ..Default::default() };
                pixmap.stroke_path(&sep_path, &sep_paint, &stroke, Transform::identity(), None);
                y_offset += 4;
            }
        }

        // Draw candidates with colored indices
        if has_candidates {
            let fg_color = Color::rgba(
                cfg.fg_color[1],
                cfg.fg_color[2],
                cfg.fg_color[3],
                cfg.fg_color[0],
            );
            let idx_color = Color::rgba(
                cfg.index_color[1],
                cfg.index_color[2],
                cfg.index_color[3],
                cfg.index_color[0],
            );

            // Render each candidate using pre-measured buffers
            let mut x_offset = pad_h as i32;
            for (idx_buf, idx_w, cand_buf, cand_w) in &cand_buffers {
                draw_buffer(
                    &mut pixmap,
                    &mut self.font_system,
                    &mut self.swash_cache,
                    idx_buf,
                    x_offset,
                    y_offset,
                    idx_color,
                );
                x_offset += *idx_w as i32;

                draw_buffer(
                    &mut pixmap,
                    &mut self.font_system,
                    &mut self.swash_cache,
                    cand_buf,
                    x_offset,
                    y_offset,
                    fg_color,
                );
                x_offset += *cand_w as i32;

                x_offset += (cfg.font_size * 0.8) as i32;
            }
        }

        let data = pixmap.data().to_vec();

        Some(RenderedFrame {
            width,
            height,
            data,
        })
    }
}

fn rounded_rect_path(w: f32, h: f32, r: f32) -> tiny_skia::Path {
    let mut pb = PathBuilder::new();
    pb.move_to(r, 0.0);
    pb.line_to(w - r, 0.0);
    pb.quad_to(w, 0.0, w, r);
    pb.line_to(w, h - r);
    pb.quad_to(w, h, w - r, h);
    pb.line_to(r, h);
    pb.quad_to(0.0, h, 0.0, h - r);
    pb.line_to(0.0, r);
    pb.quad_to(0.0, 0.0, r, 0.0);
    pb.finish().unwrap()
}

fn measure_buffer_width(buffer: &Buffer) -> f32 {
    let mut max_w: f32 = 0.0;
    for run in buffer.layout_runs() {
        let w = run.line_w;
        if w > max_w {
            max_w = w;
        }
    }
    max_w
}

fn draw_buffer(
    pixmap: &mut Pixmap,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    buffer: &Buffer,
    x_offset: i32,
    y_offset: i32,
    color: Color,
) {
    for run in buffer.layout_runs() {
        for glyph in run.glyphs.iter() {
            let physical = glyph.physical((x_offset as f32, y_offset as f32), 1.0);

            let image = swash_cache.get_image(font_system, physical.cache_key);
            let image = match image.as_ref() {
                Some(img) => img,
                None => continue,
            };

            let gx = physical.x + image.placement.left;
            let gy = physical.y - image.placement.top + run.line_y as i32;

            let [ca, cr, cg, cb] = [color.a(), color.r(), color.g(), color.b()];

            for row in 0..image.placement.height as i32 {
                let py = gy + row;
                if py < 0 || py >= pixmap.height() as i32 {
                    continue;
                }
                for col in 0..image.placement.width as i32 {
                    let px = gx + col;
                    if px < 0 || px >= pixmap.width() as i32 {
                        continue;
                    }
                    let idx = (row * image.placement.width as i32 + col) as usize;
                    let alpha = image.data.get(idx).copied().unwrap_or(0) as u32;
                    if alpha == 0 {
                        continue;
                    }

                    let pixel_idx = ((py as u32 * pixmap.width() + px as u32) * 4) as usize;
                    let pixels = pixmap.data_mut();

                    // Alpha blend: src over dst (pre-multiplied)
                    let sa = (alpha * ca as u32) / 255;
                    let sr = (alpha * cr as u32) / 255;
                    let sg = (alpha * cg as u32) / 255;
                    let sb = (alpha * cb as u32) / 255;

                    let da = pixels[pixel_idx + 3] as u32;
                    let dr = pixels[pixel_idx] as u32;
                    let dg = pixels[pixel_idx + 1] as u32;
                    let db = pixels[pixel_idx + 2] as u32;

                    let inv_sa = 255 - sa;
                    pixels[pixel_idx] = ((sr + dr * inv_sa / 255).min(255)) as u8;
                    pixels[pixel_idx + 1] = ((sg + dg * inv_sa / 255).min(255)) as u8;
                    pixels[pixel_idx + 2] = ((sb + db * inv_sa / 255).min(255)) as u8;
                    pixels[pixel_idx + 3] = ((sa + da * inv_sa / 255).min(255)) as u8;
                }
            }
        }
    }
}
