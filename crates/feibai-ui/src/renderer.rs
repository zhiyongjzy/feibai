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
    pub highlight_color: [u8; 4],
    pub padding: u32,
    pub max_candidates: usize,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 22.0,
            bg_color: [240, 40, 40, 48],       // near-black semi-transparent
            fg_color: [255, 230, 230, 230],    // light gray
            preedit_color: [255, 140, 140, 140], // dimmer gray
            highlight_color: [255, 80, 160, 240], // blue highlight
            padding: 6,
            max_candidates: 9,
        }
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

        // Build candidate text: "1.你 2.妮 3.尼 ..."
        let candidate_text: String = candidates
            .iter()
            .take(cfg.max_candidates)
            .enumerate()
            .map(|(i, c)| format!("{}.{}", i + 1, c.text))
            .collect::<Vec<_>>()
            .join("  ");

        // Measure preedit line
        let mut preedit_buf = Buffer::new(&mut self.font_system, metrics);
        preedit_buf.set_size(&mut self.font_system, Some(800.0), None);
        preedit_buf.set_text(&mut self.font_system, preedit, attrs, Shaping::Advanced);
        preedit_buf.shape_until_scroll(&mut self.font_system, false);

        // Measure candidate line
        let mut cand_buf = Buffer::new(&mut self.font_system, metrics);
        cand_buf.set_size(&mut self.font_system, Some(800.0), None);
        cand_buf.set_text(
            &mut self.font_system,
            &candidate_text,
            attrs,
            Shaping::Advanced,
        );
        cand_buf.shape_until_scroll(&mut self.font_system, false);

        // Calculate dimensions
        let preedit_width = measure_buffer_width(&preedit_buf);
        let cand_width = measure_buffer_width(&cand_buf);
        let content_width = preedit_width.max(cand_width);

        let pad = cfg.padding;
        let width = (content_width as u32 + pad * 2).max(60);
        let mut height = pad * 2;
        if !preedit.is_empty() {
            height += cfg.line_height as u32;
        }
        if !candidates.is_empty() {
            height += cfg.line_height as u32;
        }

        // Create pixmap
        let mut pixmap = Pixmap::new(width, height)?;

        // Draw background
        let bg = &cfg.bg_color;
        let mut paint = Paint::default();
        paint.set_color_rgba8(bg[1], bg[2], bg[3], bg[0]);
        paint.anti_alias = true;

        let rect_path = {
            let mut pb = PathBuilder::new();
            let r = 4.0; // corner radius
            let w = width as f32;
            let h = height as f32;
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
        };
        pixmap.fill_path(&rect_path, &paint, FillRule::Winding, Transform::identity(), None);

        // Draw preedit text
        let mut y_offset = pad as i32;
        if !preedit.is_empty() {
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
                pad as i32,
                y_offset,
                color,
            );
            y_offset += cfg.line_height as i32;
        }

        // Draw candidates
        if !candidates.is_empty() {
            let color = Color::rgba(
                cfg.fg_color[1],
                cfg.fg_color[2],
                cfg.fg_color[3],
                cfg.fg_color[0],
            );
            draw_buffer(
                &mut pixmap,
                &mut self.font_system,
                &mut self.swash_cache,
                &cand_buf,
                pad as i32,
                y_offset,
                color,
            );
        }

        // Convert to ARGB8888 (tiny-skia uses RGBA pre-multiplied internally)
        let data = pixmap.data().to_vec();

        Some(RenderedFrame {
            width,
            height,
            data,
        })
    }
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
