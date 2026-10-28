//! Where the **images** on a page are drawn, and how many source pixels they carry.
//!
//! A reader uses this for one decision: how many pixels a page is worth. A scanned page
//! is a handful of photos covering almost all of it; drawing it at 2048 on the long edge
//! when the source photo is 1200 px wide produces nothing but interpolation — measured at
//! 816 ms per page against 89 ms with the source's own density (`hugG.pdf`, 43 pages).
//!
//! ## What is counted
//!
//! Only the **outermost** images: an image drawn inside a form `XObject` that is itself
//! drawn onto the page is counted where that form lands, with the product of the two
//! matrices — not once more inside the form. Form `XObject`s are walked, because a
//! producer that wraps a scanned page in a form is still a scanned page.
//!
//! ## Coordinates
//!
//! The same as the text layer: PDF user space, points, origin at the bottom left of the
//! crop box, y upwards. [`PlacedImage::bbox`] is the axis-aligned box the image's unit
//! square ends up covering, so a page turned by `/Rotate` reports the box in the
//! *unrotated* space — exactly like the text layer, which keeps the two comparable.

use crate::content::ops::TypedInstruction;
use crate::content::TypedIter;
use crate::object::Name;
use crate::object::Rect;
use crate::object::dict::keys::{HEIGHT, MATRIX, RESOURCES, SUBTYPE, WIDTH};
use crate::page::{Page, Resources};

/// A 2D affine matrix in PDF order: `[a b c d e f]`, i.e. `x' = a·x + c·y + e`.
///
/// Kept local (rather than pulling `kurbo` into this crate) because it is the only
/// place in `hayro-syntax` that has to multiply matrices, and because it must stay
/// usable without the `std` feature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix(pub [f64; 6]);

impl Matrix {
    /// The identity.
    pub const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    /// `self * rhs`: the map `rhs` first, then `self`.
    pub fn then(self, rhs: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = rhs.0;
        Self([
            a * a2 + c * b2,
            b * a2 + d * b2,
            a * c2 + c * d2,
            b * c2 + d * d2,
            a * e2 + c * f2 + e,
            b * e2 + d * f2 + f,
        ])
    }

    /// Map one point.
    pub fn map(self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }
}

/// One image drawn on a page.
#[derive(Clone, Copy, Debug)]
pub struct PlacedImage {
    /// The image's native size in pixels (its `/Width` and `/Height`).
    pub width: u32,
    /// The image's native height in pixels.
    pub height: u32,
    /// The box the image's unit square covers on the page, in PDF user space.
    ///
    /// The image's own coordinate space runs from `(0, 0)` at the bottom left to
    /// `(1, 1)` at the top right (ISO 32000-1 §8.9.5.2), so this is that square run
    /// through the transform in force when it was drawn. A skew or a rotation makes the
    /// shape a parallelogram; this reports the upright box that contains it, which is
    /// what a "how much of the page does it cover" question needs.
    pub bbox: Rect,
}

/// How deeply form `XObject` are followed. A form that draws a form that draws a form…
/// is real (report generators nest them), but a page tree that recurses forever is not,
/// and each level costs a walk of a content stream.
pub const MAX_NESTING: usize = 8;

impl Page<'_> {
    /// Every image drawn on this page, in content-stream order.
    pub fn placed_images(&self) -> Vec<PlacedImage> {
        self.placed_images_with_limit(MAX_NESTING)
    }

    /// [`Page::placed_images`] with the form-XObject nesting budget spelled out.
    pub fn placed_images_with_limit(&self, limit: usize) -> Vec<PlacedImage> {
        let mut images = Vec::new();
        walk(self, self.resources(), Matrix::IDENTITY, limit, &mut images);
        images
    }
}

/// Recurse through one content stream from raw bytes, with the resources that stream
/// sees. This is what a form `XObject` needs: its own `/Resources` (or the page's, when it
/// has none) and its own content.
fn walk_stream(
    resources: &Resources<'_>,
    data: &[u8],
    start: Matrix,
    budget: usize,
    out: &mut Vec<PlacedImage>,
) {
    let mut ops = TypedIter::new(data);
    walk_ops(resources, &mut ops, start, budget, out);
}

/// Recurse through one content stream.
///
/// Only the current transform matrix matters here: `q`/`Q` save and restore it, `cm`
/// concatenates onto it, and `Do` either draws an image (record it) or a form (walk it
/// with the current matrix).
fn walk(
    page: &Page<'_>,
    resources: &Resources<'_>,
    start: Matrix,
    budget: usize,
    out: &mut Vec<PlacedImage>,
) {
    let mut ops = page.typed_operations();
    walk_ops(resources, &mut ops, start, budget, out);
}

fn walk_ops(
    resources: &Resources<'_>,
    ops: &mut TypedIter<'_>,
    start: Matrix,
    budget: usize,
    out: &mut Vec<PlacedImage>,
) {
    let mut ctm = start;
    let mut stack: Vec<Matrix> = Vec::new();

    while let Some(instruction) = ops.next() {
        match instruction {
            TypedInstruction::SaveState(_) => stack.push(ctm),
            TypedInstruction::RestoreState(_) => {
                if let Some(saved) = stack.pop() {
                    ctm = saved;
                }
            }
            TypedInstruction::Transform(t) => {
                let a = t.0.as_f64();
                let b = t.1.as_f64();
                let c = t.2.as_f64();
                let d = t.3.as_f64();
                let e = t.4.as_f64();
                let f = t.5.as_f64();
                ctm = ctm.then(Matrix([a, b, c, d, e, f]));
            }
            TypedInstruction::XObject(name) => {
                let Some(object) = resources.get_x_object(name.0) else {
                    continue;
                };
                match object
                    .dict()
                    .get::<Name<'_>>(SUBTYPE)
                    .as_deref()
                    .and_then(|name| core::str::from_utf8(name).ok())
                {
                    Some("Image") => {
                        let dict = object.dict();
                        let width = dict.get::<u32>(WIDTH).unwrap_or(0);
                        let height = dict.get::<u32>(HEIGHT).unwrap_or(0);
                        if width > 0 && height > 0 {
                            out.push(PlacedImage {
                                width,
                                height,
                                bbox: unit_square(ctm),
                            });
                        }
                    }
                    Some("Form") => {
                        if budget == 0 {
                            continue;
                        }
                        // A form's `/Matrix` places its own coordinate space; its
                        // `/BBox` and `/Resources` are what it draws with (a form
                        // without `/Resources` inherits the caller's, ISO 32000-1
                        // §8.10.1).
                        let matrix = object
                            .dict()
                            .get::<[f64; 6]>(MATRIX)
                            .map_or(Matrix::IDENTITY, Matrix);
                        let owned;
                        let inner = match object
                            .dict()
                            .get::<crate::object::Dict<'_>>(RESOURCES)
                        {
                            Some(dict) => {
                                owned = Resources::new(dict);
                                &owned
                            }
                            None => resources,
                        };
                        let Ok(data) = object.decoded() else {
                            continue;
                        };
                        walk_stream(
                            inner,
                            data.as_ref(),
                            ctm.then(matrix),
                            budget - 1,
                            out,
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The unit square `[0, 1]²` of an image's own space, after `ctm`.
fn unit_square(ctm: Matrix) -> Rect {
    let corners = [
        ctm.map(0.0, 0.0),
        ctm.map(1.0, 0.0),
        ctm.map(0.0, 1.0),
        ctm.map(1.0, 1.0),
    ];
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (x, y) in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    Rect { x0, y0, x1, y1 }
}
