//! The text layer: characters in content-stream order, each with the box its ink covers.

use std::path::{Path, PathBuf};

use hayro_interpret::text::{TextLayerRecorder, extract_text};
use hayro_interpret::{
    Context, InterpreterCache, InterpreterSettings, interpret_page, kurbo::Affine, kurbo::Rect,
};
use hayro_syntax::Pdf;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/pdfs")
        .join(name)
}

fn open(name: &str) -> (Vec<u8>, Pdf) {
    let bytes = std::fs::read(fixture(name)).expect("fixture");
    let pdf = Pdf::new(bytes.clone()).expect("opens");
    (bytes, pdf)
}

#[test]
fn glyph_runs_come_out_with_text_and_boxes() {
    let (_, pdf) = open("text/glyph_names.pdf");
    let text = extract_text(&pdf.pages()[0], &InterpreterSettings::default());
    assert!(!text.is_empty(), "a text page must yield glyphs");
    assert!(
        !text.text().is_empty(),
        "each glyph of this fixture has a glyph name, so Unicode must come out"
    );

    // A glyph with a box sits where the producer put it: points on this fixture's
    // 100×100 page, not em units (which would be a tenth of that) and not pixels.
    let glyphs_with_box: Vec<_> = text.glyphs.iter().filter(|g| g.bbox.is_some()).collect();
    assert!(
        glyphs_with_box.len() > text.glyphs.len() / 2,
        "most of a text page is visible ink; got {}/{}",
        glyphs_with_box.len(),
        text.glyphs.len()
    );
    for glyph in glyphs_with_box {
        let bbox = glyph.bbox.expect("filtered");
        assert!(bbox.x1 > bbox.x0 && bbox.y1 > bbox.y0, "{bbox:?}");
        assert!(bbox.x1 - bbox.x0 < 200.0, "width in points: {bbox:?}");
        assert!((0.0..100.0).contains(&bbox.y0), "{bbox:?}");
    }

    // A space is mapped to Unicode but draws no ink — it must not get a zero-width box,
    // which would put a selection handle in the middle of a word gap.
    let spaces: Vec<_> = text
        .glyphs
        .iter()
        .filter(|g| g.text.as_deref() == Some(" "))
        .collect();
    assert!(!spaces.is_empty(), "this fixture has word gaps");
    for space in spaces {
        assert!(space.bbox.is_none(), "{space:?}");
    }

    // The advance is the displacement to the next glyph: a horizontal font moves along
    // +x, never backwards through a run.
    let runs_with_advance: Vec<_> = text
        .glyphs
        .windows(2)
        .filter_map(|pair| pair[0].advance)
        .collect();
    assert!(!runs_with_advance.is_empty());
    for (dx, _) in runs_with_advance {
        assert!(dx >= 0.0, "a left-to-right run must not go backwards: {dx}");
    }
    assert!(
        text.glyphs.last().is_some_and(|last| last.advance.is_none()),
        "the pen after the last glyph of the page is not observable"
    );
}

#[test]
fn the_recorder_matches_the_one_call_helper() {
    let (_, pdf) = open("text/glyph_names.pdf");
    let settings = InterpreterSettings::default();
    let page = &pdf.pages()[0];

    let cache = InterpreterCache::new();
    let bbox = Rect::new(0.0, 0.0, 8192.0, 8192.0);
    let mut context = Context::new(Affine::IDENTITY, bbox, &cache, page.xref(), settings.clone());
    let mut recorder = TextLayerRecorder::new();
    interpret_page(page, &mut context, &mut recorder);
    let manual = recorder.into_text();

    let helper = extract_text(page, &settings);
    assert_eq!(manual.text(), helper.text());
    assert_eq!(manual.glyphs.len(), helper.glyphs.len());
}

#[test]
fn a_page_without_text_records_nothing() {
    // `clip_path_nested.pdf` draws paths and clips only.
    let bytes = std::fs::read(fixture("custom/clip_path_nested.pdf")).expect("fixture");
    let pdf = Pdf::new(bytes).expect("opens");
    let empty = pdf
        .pages()
        .iter()
        .all(|page| extract_text(page, &InterpreterSettings::default()).is_empty());
    assert!(empty, "vector-only pages must not invent glyphs");
}
