//! The document outline — the bookmark tree a reader shows as a table of contents.
//!
//! PDF keeps it as a linked list hung off the catalog (`/Outlines`), not as an array:
//! each item has `/First`, `/Last`, `/Next`, `/Prev` and `/Parent`, and the nesting is
//! expressed by those links. Reading it therefore means walking pointers, with a
//! visited set — a producer that writes a cyclic `/Next` would otherwise hang the walk.
//!
//! ## Where an item points
//!
//! An item's target is a **destination**, spelled three ways in the wild:
//!
//! - `/Dest [page /Fit]` — the page as an indirect reference or as a page index;
//! - `/Dest (name)` — a **named** destination, looked up in `/Names /Dests` or, for
//!   compatibility with PDF 1.1 producers, in the catalog's `/Dests` dictionary;
//! - `/A << /S /GoTo /D … >>` — an action carrying the same destination.
//!
//! All three end up as a page number here. An item whose target cannot be resolved to a
//! page of this document is reported with `page: None` rather than dropped: a reader
//! that wants to hide it can, and one that wants to show the title as a plain label can
//! too.
//!
//! Named destinations may also point at a **structure element** (the `/SD` key) instead
//! of a page; those are not resolved here (see [`Dest`]).

use alloc::string::String;
use alloc::vec::Vec;

use crate::object::dict::keys::*;
use crate::object::{Dict, Object, ObjRef};
use crate::xref::XRef;

/// One entry of the outline, with its children.
#[derive(Clone, Debug)]
pub struct OutlineItem {
    /// The title as written (already decoded from `PDFDocEncoding` / UTF-16).
    pub title: String,
    /// The destination this item jumps to, when it has one that could be resolved.
    pub dest: Option<Dest>,
    /// The nested items, in document order.
    pub children: Vec<Self>,
}

/// Where an outline item or a named destination points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dest {
    /// The 0-based page index.
    pub page: usize,
    /// The destination's view mode (`/Fit`, `/XYZ`, …), kept verbatim: a reader that
    /// wants to restore the scroll position needs the coordinates that follow it, and
    /// those are left in the raw destination array.
    pub page_object: ObjRef,
}

/// Walk the outline of a document, in document order, depth first.
///
/// Returns an empty list when the document has no `/Outlines`, and also when the link
/// structure is unreadable — an outline is a convenience, and a broken one must not make
/// the whole document fail to open.
pub fn outline(xref: &XRef) -> Vec<OutlineItem> {
    let Some(catalog) = catalog(xref) else {
        return Vec::new();
    };
    let Some(root) = catalog.get::<Dict<'_>>(OUTLINES) else {
        return Vec::new();
    };
    let mut visited = Vec::new();
    let mut items = Vec::new();
    let mut current: Option<Dict<'_>> = root.get::<Dict<'_>>(FIRST);
    while let Some(node) = current {
        let Some(id) = node.obj_id() else {
            break;
        };
        if visited.contains(&id) {
            break;
        }
        visited.push(id);
        items.push(item(&node, xref, &mut visited));
        current = node.get::<Dict<'_>>(NEXT);
    }
    items
}

/// The catalog (`/Root → /Pages`'s parent). Reached through the trailer's `/Root`.
fn catalog(xref: &XRef) -> Option<Dict<'_>> {
    xref.get::<Dict<'_>>(xref.root_id())
}

fn item(
    dict: &Dict<'_>,
    xref: &XRef,
    visited: &mut Vec<crate::object::ObjectIdentifier>,
) -> OutlineItem {
    let title = dict
        .get::<crate::object::String<'_>>(TITLE)
        .map(|s| decode_text_string(s.as_bytes()))
        .unwrap_or_default();
    let dest = destination(dict, xref);
    let mut children = Vec::new();
    if let Some(first) = dict.get::<Dict<'_>>(FIRST) {
        let mut current = Some(first);
        while let Some(child) = current {
            let Some(id) = child.obj_id() else {
                break;
            };
            if visited.contains(&id) {
                break;
            }
            visited.push(id);
            children.push(item(&child, xref, visited));
            current = child.get::<Dict<'_>>(NEXT);
        }
    }
    OutlineItem {
        title,
        dest,
        children,
    }
}

/// Resolve an item's target: `/Dest`, or `/A` with `S = /GoTo`.
fn destination(dict: &Dict<'_>, xref: &XRef) -> Option<Dest> {
    if let Some(dest) = dict.get::<Object<'_>>(DEST)
        && let Some(resolved) = resolve_dest(&dest, xref)
    {
        return Some(resolved);
    }
    let action = dict.get::<Dict<'_>>(A)?;
    let kind = action.get::<crate::object::Name<'_>>(S)?;
    if kind.as_str() != "GoTo" {
        return None;
    }
    let dest = action.get::<Object<'_>>(D)?;
    resolve_dest(&dest, xref)
}

/// A destination object → the page it lands on.
fn resolve_dest(dest: &Object<'_>, xref: &XRef) -> Option<Dest> {
    match dest {
        Object::Array(array) => dest_to_page(array, xref),
        Object::String(name) => named_dest(name.as_bytes(), xref),
        Object::Name(name) => named_dest(name.as_ref(), xref),
        _ => None,
    }
}

/// `[page_ref /Fit …]` where the first element is either an indirect reference to a
/// page or (legacy) a page index.
fn dest_to_page(array: &crate::object::Array<'_>, xref: &XRef) -> Option<Dest> {
    // A destination whose first element is not a reference (a legacy bare page index)
    // has no page object to point at; the array reader hands that back as `NotRef`.
    let page_object = array.raw_iter().next()?.as_obj_ref()?;
    let page = xref.get::<Dict<'_>>(page_object.into())?;
    let index = page_index(xref, &page)?;
    Some(Dest { page: index, page_object })
}

/// Look a named destination up in `/Names /Dests`, then in the catalog's `/Dests`.
fn named_dest(name: &[u8], xref: &XRef) -> Option<Dest> {
    let catalog = catalog(xref)?;

    // Modern shape: `/Names << /Dests << /Names [ (name) [dest] … ] >> >>`, which may
    // name a `/Kids` tree instead of the array.
    if let Some(names) = catalog.get::<Dict<'_>>(NAMES)
        && let Some(dests) = names.get::<Dict<'_>>(DESTS)
    {
        if let Some(target) = lookup_name_tree(&dests, name, xref, 0) {
            return resolve_dest(&target, xref);
        }
        // A dict that is not a name tree but a plain mapping.
        if let Some(object) = dests.get::<Object<'_>>(name) {
            return resolve_dest(&object, xref);
        }
    }

    // PDF 1.1 shape: `/Dests << (name) [dest] … >>` directly on the catalog.
    let legacy = catalog.get::<Dict<'_>>(DESTS)?;
    let object = legacy.get::<Object<'_>>(name)?;
    resolve_dest(&object, xref)
}

/// Walk a name tree (`/Names` array or `/Kids`), looking for `name`.
fn lookup_name_tree<'a>(
    dict: &Dict<'a>,
    name: &[u8],
    xref: &'a XRef,
    depth: usize,
) -> Option<Object<'a>> {
    if depth > 32 {
        return None;
    }
    if let Some(array) = dict.get::<crate::object::Array<'a>>(NAMES) {
        // A name tree alternates key and value; keys are always direct strings, values
        // may be indirect. Reading the two interleaved through `raw_iter` keeps the
        // pairing, and the value is resolved through the xref as it goes.
        let mut iter = array.raw_iter();
        while let Some(key) = iter.next() {
            let Some(value) = iter.next() else {
                break;
            };
            let crate::object::MaybeRef::NotRef(Object::String(text)) = key else {
                continue;
            };
            if text.as_bytes() != name {
                continue;
            }
            return match value {
                crate::object::MaybeRef::Ref(r) => xref.get::<Object<'a>>(r.into()),
                crate::object::MaybeRef::NotRef(object) => Some(object),
            };
        }
    }
    if let Some(kids) = dict.get::<crate::object::Array<'a>>(KIDS) {
        for kid in kids.iter::<Dict<'a>>() {
            if let Some(found) = lookup_name_tree(&kid, name, xref, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

/// The 0-based index of a page object in the page tree.
fn page_index(xref: &XRef, page: &Dict<'_>) -> Option<usize> {
    let catalog = catalog(xref)?;
    let pages = catalog.get::<Dict<'_>>(PAGES)?;
    let mut index = 0;
    let mut stack = vec![pages];
    while let Some(node) = stack.pop() {
        if node.get::<crate::object::Name<'_>>(TYPE).as_deref() == Some(b"Pages") {
            if let Some(kids) = node.get::<crate::object::Array<'_>>(KIDS) {
                // `pop` walks the rightmost kid first; push in reverse so that the
                // traversal is in document order.
                let children: Vec<Dict<'_>> = kids.iter::<Dict<'_>>().collect();
                for child in children.into_iter().rev() {
                    stack.push(child);
                }
            }
        } else {
            if node.obj_id() == page.obj_id() {
                return Some(index);
            }
            index += 1;
        }
    }
    None
}

/// Decode a PDF text string: UTF-16 when it starts with a BOM, `PDFDocEncoding` otherwise.
///
/// `PDFDocEncoding` differs from Latin-1 in a dozen code points (`•` at 0x80 and so on);
/// for the handful of characters that show up in bookmarks — quotes, dashes, bullets —
/// this maps the ones that differ and leaves the rest as Latin-1, which is right for
/// ASCII and nearly right elsewhere. A bookmark title is a label: a bullet rendered as
/// its `PDFDocEncoding` stand-in is visible but not harmful, and a full table is a
/// dependency this crate does not want.
fn decode_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        return String::from_utf16_lossy(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect::<Vec<_>>(),
        );
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        return String::from_utf16_lossy(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>(),
        );
    }
    bytes.iter().map(|byte| pdfdoc_char(*byte)).collect()
}

/// One `PDFDocEncoding` byte → the character it stands for.
fn pdfdoc_char(byte: u8) -> char {
    const HIGH: [(u8, char); 24] = [
        (0x18, '\u{02D8}'),
        (0x19, '\u{02C7}'),
        (0x1A, '\u{02C6}'),
        (0x1B, '\u{02D9}'),
        (0x1C, '\u{02DD}'),
        (0x1D, '\u{02DB}'),
        (0x1E, '\u{02DA}'),
        (0x1F, '\u{02DC}'),
        (0x80, '\u{2022}'),
        (0x81, '\u{2020}'),
        (0x82, '\u{2021}'),
        (0x83, '\u{2026}'),
        (0x84, '\u{2014}'),
        (0x85, '\u{2013}'),
        (0x86, '\u{0192}'),
        (0x87, '\u{2044}'),
        (0x88, '\u{2039}'),
        (0x89, '\u{203A}'),
        (0x8A, '\u{2212}'),
        (0x8B, '\u{2030}'),
        (0x8C, '\u{201E}'),
        (0x8D, '\u{201C}'),
        (0x8E, '\u{201D}'),
        (0x8F, '\u{2018}'),
    ];
    if let Some((_, ch)) = HIGH.iter().find(|(b, _)| *b == byte) {
        return *ch;
    }
    byte as char
}
