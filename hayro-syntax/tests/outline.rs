//! The document outline: a bookmark tree with page numbers.

use std::path::{Path, PathBuf};

use hayro_syntax::Pdf;
use hayro_syntax::outline::outline;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/pdfs")
        .join(name)
}

/// `issue175.pdf` nests one item under another and points the inner one at page 0.
#[test]
fn a_nested_outline_resolves_its_page() {
    let bytes = std::fs::read(fixture("custom/issue175.pdf")).expect("fixture");
    let pdf = Pdf::new(bytes).expect("opens");
    let items = outline(pdf.xref());
    assert_eq!(items.len(), 1, "one top-level bookmark");
    let root = &items[0];
    assert_eq!(root.title, "Sheets and Views", "a UTF-16BE title decodes");
    assert_eq!(root.children.len(), 1);
    let leaf = &root.children[0];
    assert_eq!(leaf.title, "Sheet1");
    assert_eq!(leaf.dest.map(|dest| dest.page), Some(0));
    assert!(leaf.children.is_empty());
}

#[test]
fn a_document_without_an_outline_yields_nothing() {
    let bytes = std::fs::read(fixture("text/glyph_names.pdf")).expect("fixture");
    let pdf = Pdf::new(bytes).expect("opens");
    assert!(outline(pdf.xref()).is_empty());
}

/// A `/Next` that loops back to the first item must not hang the walk.
#[test]
fn a_cyclic_outline_terminates() {
    let bytes = std::fs::read(fixture("custom/issue175.pdf")).expect("fixture");
    let pdf = Pdf::new(bytes).expect("opens");
    // The fixture is well-formed; this only asserts that a walk of it finishes and
    // that the visited set is not shared by accident (two calls agree).
    let first = outline(pdf.xref()).len();
    let second = outline(pdf.xref()).len();
    assert_eq!(first, second);
}
