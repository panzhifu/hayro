//! Where a page draws its images, and how many pixels each one carries.

use std::path::{Path, PathBuf};

use hayro_syntax::Pdf;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/pdfs")
        .join(name)
}

#[test]
fn a_page_image_reports_its_pixels_and_footprint() {
    let bytes = std::fs::read(fixture("custom/color_space_indexed.pdf")).expect("fixture");
    let pdf = Pdf::new(bytes).expect("opens");
    let images = pdf.pages()[0].placed_images();
    assert!(!images.is_empty(), "this fixture draws an image");
    for image in &images {
        assert!(image.width > 0 && image.height > 0);
        assert!(
            image.bbox.x1 > image.bbox.x0 && image.bbox.y1 > image.bbox.y0,
            "an image covers some area: {:?}",
            image.bbox
        );
        // Points, not pixels: the footprint of a page image is a page-sized box.
        let (w, h) = (image.bbox.x1 - image.bbox.x0, image.bbox.y1 - image.bbox.y0);
        assert!(w < 5000.0 && h < 5000.0, "{w}×{h} pt");
    }
}

#[test]
fn a_blank_page_has_no_images() {
    let bytes = std::fs::read(fixture("custom/blank_page_without_contents.pdf")).expect("fixture");
    let pdf = Pdf::new(bytes).expect("opens");
    assert!(pdf.pages()[0].placed_images().is_empty());
}

#[test]
fn a_pdf_is_send_and_sync() {
    // A reader keeps one handle per open book and hands pages to whichever background
    // thread asks; that only works if the document itself crosses threads.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Pdf>();
}
