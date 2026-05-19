mod renderer;
pub use renderer::{CandidateRenderer, RenderConfig, RenderedFrame, Theme};

#[cfg(test)]
mod tests {
    use super::*;
    use feibai_core::Candidate;

    #[test]
    fn render_produces_non_empty_buffer() {
        let mut renderer = CandidateRenderer::new(RenderConfig::default());
        let candidates = vec![
            Candidate { text: "你".into(), comment: None },
            Candidate { text: "妮".into(), comment: None },
            Candidate { text: "尼".into(), comment: None },
        ];
        let frame = renderer.render("ni", &candidates, 0).unwrap();
        assert!(frame.width > 0 && frame.height > 0);
        assert_eq!(frame.data.len(), (frame.width * frame.height * 4) as usize);
    }

    #[test]
    fn render_empty_returns_none() {
        let mut renderer = CandidateRenderer::new(RenderConfig::default());
        assert!(renderer.render("", &[], 0).is_none());
    }

    #[test]
    fn render_preedit_only() {
        let mut renderer = CandidateRenderer::new(RenderConfig::default());
        let frame = renderer.render("zhong", &[], 0).unwrap();
        assert!(frame.width > 0 && frame.height > 0);
    }
}
