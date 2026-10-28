//! Turning a page's glyph runs into a **text layer**: characters plus where they sit.
//!
//! [`TextLayerRecorder`] answers the question a reader asks when the user drags a
//! selection over a paragraph or puts a highlight on a sentence — "which characters are
//! on this page, and how big is each one's box". hayro already knows both while
//! rendering: every run of positioned glyphs arrives at
//! [`Device::draw_glyph_run`](crate::Device::draw_glyph_run), and
//! [`PositionedGlyph::transform`](crate::font::PositionedGlyph::transform) carries the
//! text-space matrix of that glyph (font size folded in, origin shifted to the glyph),
//! while [`PositionedGlyph::bbox`](crate::font::PositionedGlyph::bbox) is the ink box
//! that matrix puts on the page.
//!
//! So the recorder is a [`Device`](crate::Device) that draws nothing and writes glyphs
//! down. Handing it to
//! [`interpret_page`](crate::interpret_page) is a **second pass** over the content
//! stream: the recorder owns an [`InterpreterCache`] with an `Rc` inside, which makes
//! the pass `!Send`. Folding it into the rendering pass would drag that constraint onto
//! renderers that run on several threads, and a reader that wants the text layer can
//! easily afford a second walk (it is the same order of magnitude as drawing the page,
//! not a multiple of it).
//!
//! ## What a "box" is here
//!
//! The ink of the glyph's outline, **not** an em square: a space has no ink and
//! therefore no box, and a descender's box hangs below the baseline the way the ink
//! does. That is what a selection is expected to cover.
//!
//! ## Coordinates
//!
//! Everything comes out in **PDF user space**: points, origin at the bottom left of the
//! page's crop box, y growing upwards. That is the space the content stream is written
//! in, so the numbers compare directly against what a producer put into the file.
//! Applying `/Rotate` and turning y downwards is the caller's job — it knows which box
//! its pixels follow, and [`Page::initial_transform`](hayro_syntax::page::Page::initial_transform)
//! is the matrix that does exactly that.
//!
//! ## What is left out
//!
//! - Type 3 glyphs report no box (their extent lives in a content stream of their own).
//! - Glyphs inside a hidden optional-content group are not recorded at all — they are
//!   not visible, so a selection should not find them.
//! - Invisible text (`Tr 3`, the usual shape of an OCR layer under a scan) **is**
//!   recorded: that text is exactly what a reader wants to select.

use hayro_syntax::object::Rect;

use crate::context::{Context, InterpreterCache};
use crate::device::Device;
use crate::font::{Glyph, GlyphRun, PositionedGlyph};
use crate::interpret::interpret_page;
use crate::types::{BlendMode, ClipPath, DrawMode, DrawProps, Image, ImageDrawProps};
use crate::x_object::soft_mask::SoftMask;
use crate::InterpreterSettings;

/// One glyph on a page, with the box its ink covers.
#[derive(Clone, Debug)]
pub struct TextGlyph {
    /// The Unicode this glyph stands for, when the font gives a way back to it (a
    /// `ToUnicode` map, a glyph name, …). `None` for a bare CID font without one.
    pub text: Option<String>,
    /// The character code the content stream used to select this glyph: the byte for a
    /// simple font, the code after the `CMap` for a composite one.
    pub char_code: u32,
    /// The glyph's ink, in PDF user space. `None` when it draws nothing (a space, an
    /// empty outline, a Type 3 glyph).
    pub bbox: Option<Rect>,
    /// How far the pen travels after this glyph, as a vector from its origin to the next
    /// one (PDF user space). `None` on the last glyph of a run — the pen after it is not
    /// something the run records, and guessing it from the font's advance would report a
    /// displacement that may not have happened.
    pub advance: Option<(f64, f64)>,
}

/// Everything one page's text layer yields.
#[derive(Clone, Debug, Default)]
pub struct ExtractedText {
    /// Glyphs in content-stream order — the order the page was **written**, which for a
    /// two-column layout is not the order a human reads it.
    pub glyphs: Vec<TextGlyph>,
}

impl ExtractedText {
    /// The characters of this page with nothing inserted between them.
    ///
    /// Words are **not** split here: whether two glyphs belong to one word is a question
    /// about their geometry, and answering it inside a PDF interpreter would bake one
    /// heuristic into every consumer.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for glyph in &self.glyphs {
            if let Some(text) = &glyph.text {
                out.push_str(text);
            }
        }
        out
    }

    /// Whether nothing at all was recorded (a scan, a page of pure vectors).
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

/// A device that draws nothing and remembers the glyphs it was asked to draw.
///
/// Usage is two lines — build a cache, hand the recorder to `interpret_page`:
///
/// ```
/// # use hayro_syntax::Pdf;
/// # fn demo(pdf: &Pdf, page_index: usize) {
/// use hayro_interpret::text::TextLayerRecorder;
/// use hayro_interpret::{
///     Context, InterpreterCache, InterpreterSettings, interpret_page,
/// };
/// use hayro_interpret::kurbo::{Affine, Rect};
///
/// let cache = InterpreterCache::new();
/// let bbox = Rect::new(0.0, 0.0, 8192.0, 8192.0);
/// let page = &pdf.pages()[page_index];
/// let mut context = Context::new(
///     Affine::IDENTITY,
///     bbox,
///     &cache,
///     page.xref(),
///     InterpreterSettings::default(),
/// );
/// let mut recorder = TextLayerRecorder::new();
/// interpret_page(page, &mut context, &mut recorder);
/// let text = recorder.into_text();
/// assert!(!text.text().is_empty() || text.is_empty());
/// # }
/// ```
///
/// The `bbox` is only the device's idea of a drawable area. This recorder ignores it, so
/// the value is only relevant to implementations that clip: pass anything that contains
/// the page.
pub struct TextLayerRecorder {
    glyphs: Vec<TextGlyph>,
}

impl Default for TextLayerRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl TextLayerRecorder {
    /// A recorder with nothing in it.
    pub fn new() -> Self {
        Self { glyphs: Vec::new() }
    }

    /// The collected glyphs, in the order they were drawn.
    pub fn into_text(self) -> ExtractedText {
        ExtractedText {
            glyphs: self.glyphs,
        }
    }
}

impl<'a> Device<'a> for TextLayerRecorder {
    fn draw_path(&mut self, _: &kurbo::BezPath, _: DrawProps<'a>, _: &DrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        _: DrawProps<'a>,
        _: &DrawMode,
    ) {
        let glyphs = glyph_run.glyphs();
        for (index, glyph) in glyphs.iter().enumerate() {
            self.glyphs.push(TextGlyph {
                text: unicode_of(glyph),
                char_code: char_code_of(glyph),
                bbox: glyph.bbox().map(|rect| Rect {
                    x0: rect.x0,
                    y0: rect.y0,
                    x1: rect.x1,
                    y1: rect.y1,
                }),
                advance: glyphs.get(index + 1).map(|next| {
                    let here = glyph.transform().translation();
                    let there = next.transform().translation();
                    (there.x - here.x, there.y - here.y)
                }),
            });
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

/// The Unicode text of a glyph as a `String`, when the font maps it back at all.
fn unicode_of(glyph: &PositionedGlyph<'_>) -> Option<String> {
    match match &**glyph {
        Glyph::Outline(outline) => outline.as_unicode(),
        Glyph::Type3(type3) => type3.as_unicode(),
    }? {
        hayro_cmap::BfString::Char(c) => {
            let mut out = String::new();
            out.push(c);
            Some(out)
        }
        hayro_cmap::BfString::String(s) => Some(s),
    }
}

/// The character code behind a glyph — the byte for a simple font, the code after the
/// `CMap` for a composite one.
fn char_code_of(glyph: &PositionedGlyph<'_>) -> u32 {
    match &**glyph {
        Glyph::Outline(outline) => outline.char_code(),
        Glyph::Type3(type3) => type3.char_code(),
    }
}

/// The text layer of a page, in one call.
///
/// [`Page::extract_text`](hayro_syntax::page::Page::extract_text) is the same walk from
/// the page's side; this one exists so a caller that already has the settings at hand
/// (an unembedded-font resolver, say) does not have to go through the default.
pub fn extract_text(
    page: &hayro_syntax::page::Page<'_>,
    settings: &InterpreterSettings,
) -> ExtractedText {
    let cache = InterpreterCache::new();
    // This recorder never clips, so the area is irrelevant; keep it finite so that an
    // implementation that does clip has something sane to clip to.
    let bbox = kurbo::Rect::new(0.0, 0.0, 8192.0, 8192.0);
    let mut context = Context::new(
        kurbo::Affine::IDENTITY,
        bbox,
        &cache,
        page.xref(),
        settings.clone(),
    );
    let mut recorder = TextLayerRecorder::new();
    interpret_page(page, &mut context, &mut recorder);
    recorder.into_text()
}
