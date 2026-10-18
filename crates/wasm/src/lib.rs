//! PdfCraft as a lean, headless WebAssembly library for PDF byte jobs in web hosts (run it in a
//! Web Worker): merge, extract pages, images to PDF, compress, ink and markup annotations, and a
//! full-page image overlay.
//!
//! It depends on the object layer and the few feature crates these jobs need, not on
//! `pdfcraft-engine` (which brings the JavaScript interpreter, XFA, OCR and signing). The logic
//! is plain Rust ([`merge`], [`extract`] …), tested natively; the `#[wasm_bindgen]` functions at
//! the bottom only convert arguments. A panic aborts on wasm (there is no unwinding), so nothing
//! here may panic; the host restarts the worker if the module traps.
//!
//! Time: wasm has no clock, so functions that stamp dates take the caller's `Date.now()`
//! (milliseconds since the Unix epoch).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use std::sync::Arc;

use pdfcraft_annot::{LineEnding, Meta, NewAnnotation, Rgb, Shape, Style};
use pdfcraft_cos::{Document, SaveOptions, pdf_date, write_full, write_incremental};
use pdfcraft_edit::{AddedImage, Content};
use serde_json::Value;
use wasm_bindgen::prelude::*;

/// Largest input accepted, per PDF or image and in total (200 MB).
pub const MAX_INPUT: usize = 200 * 1024 * 1024;
/// Most files one merge or images-to-PDF call accepts.
pub const MAX_FILES: usize = 500;
/// Most annotations one `add_ink` call accepts.
pub const MAX_ITEMS: usize = 10_000;
/// Most points across all strokes of one `add_ink` call.
pub const MAX_POINTS: usize = 1_000_000;

/// The library version (the PdfCraft workspace version).
pub fn library_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn check_size(len: usize, what: &str) -> Result<(), String> {
    if len > MAX_INPUT { Err(format!("{what} is larger than {} MB", MAX_INPUT / (1024 * 1024))) } else { Ok(()) }
}

fn check_total(parts: &[&[u8]]) -> Result<(), String> {
    let total = parts.iter().fold(0usize, |a, p| a.saturating_add(p.len()));
    check_size(total, "the input")
}

/// Run `f`, turning an escaped panic into an error on native targets (on wasm a panic aborts).
fn guard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err("internal error while processing the PDF".into()))
}

fn open(bytes: &[u8]) -> Result<Document, String> {
    check_size(bytes.len(), "the PDF")?;
    if bytes.is_empty() {
        return Err("the PDF is empty".into());
    }
    Document::open(Arc::new(bytes.to_vec())).map_err(|e| format!("could not open the PDF: {e}"))
}

fn save_new(doc: &Document) -> Result<Vec<u8>, String> {
    write_full(doc, &SaveOptions::default()).map_err(|e| format!("could not write the PDF: {e}"))
}

/// Save an edited document, appending to the original (incremental), with `now_ms` as ModDate.
fn save_edited(doc: &Document, now_ms: f64) -> Result<Vec<u8>, String> {
    let opts = SaveOptions { mod_date: date(now_ms), ..SaveOptions::default() };
    write_incremental(doc, &opts).or_else(|_| write_full(doc, &opts)).map_err(|e| format!("could not write the PDF: {e}"))
}

fn date(now_ms: f64) -> Option<String> {
    if now_ms.is_finite() && now_ms > 0.0 && now_ms < 1.0e15 { Some(pdf_date((now_ms / 1000.0) as i64)) } else { None }
}

fn page_index(doc: &Document, page: u32) -> Result<usize, String> {
    let n = pdfcraft_organize::page_count(doc).map_err(|e| e.to_string())?;
    let i = usize::try_from(page).map_err(|e| e.to_string())?;
    if i >= n { Err(format!("page {i} does not exist (the PDF has {n} pages, numbered from 0)")) } else { Ok(i) }
}

/// The number of pages.
pub fn page_count(bytes: &[u8]) -> Result<u32, String> {
    guard(|| {
        let doc = open(bytes)?;
        let n = pdfcraft_organize::page_count(&doc).map_err(|e| e.to_string())?;
        u32::try_from(n).map_err(|e| e.to_string())
    })
}

/// Combine PDFs, in order, into one.
pub fn merge(inputs: &[&[u8]]) -> Result<Vec<u8>, String> {
    guard(|| {
        if inputs.is_empty() {
            return Err("no PDFs to merge".into());
        }
        if inputs.len() > MAX_FILES {
            return Err(format!("at most {MAX_FILES} PDFs can be merged at once"));
        }
        check_total(inputs)?;
        let docs = inputs.iter().map(|b| open(b)).collect::<Result<Vec<_>, _>>()?;
        let titles: Vec<String> = (1..=docs.len()).map(|i| format!("Document {i}")).collect();
        let sources: Vec<(&str, &Document)> = titles.iter().map(String::as_str).zip(docs.iter()).collect();
        let out = pdfcraft_organize::combine(&sources).map_err(|e| e.to_string())?;
        save_new(&out)
    })
}

/// A new PDF of `pages` (0-based, in that order; repeats allowed).
pub fn extract(bytes: &[u8], pages: &[u32]) -> Result<Vec<u8>, String> {
    guard(|| {
        if pages.is_empty() {
            return Err("no pages chosen".into());
        }
        if pages.len() > 100_000 {
            return Err("too many pages chosen".into());
        }
        let doc = open(bytes)?;
        let idx = pages.iter().map(|p| page_index(&doc, *p)).collect::<Result<Vec<_>, _>>()?;
        let out = pdfcraft_organize::extract_pages(&doc, &idx).map_err(|e| e.to_string())?;
        save_new(&out)
    })
}

/// Page sizes for [`images_to_pdf`]: `None`/`"fit"` makes each page its image's size.
fn page_size(name: &str) -> Result<Option<(f64, f64)>, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "" | "fit" => Ok(None),
        "a4" => Ok(Some((595.276, 841.89))),
        "letter" => Ok(Some((612.0, 792.0))),
        other => Err(format!("unknown page size `{other}` (fit, a4 or letter)")),
    }
}

/// One page per image (PNG, JPEG, JPEG 2000, TIFF, GIF or BMP). `page_size` is `fit` (default:
/// the image's own size), `a4` or `letter` (the image centred and scaled to fit a 1/2-inch
/// margin, turned to landscape when the image is wider than tall).
pub fn images_to_pdf(images: &[&[u8]], size: Option<&str>) -> Result<Vec<u8>, String> {
    guard(|| {
        if images.is_empty() {
            return Err("no images".into());
        }
        if images.len() > MAX_FILES {
            return Err(format!("at most {MAX_FILES} images at once"));
        }
        check_total(images)?;
        let named: Vec<(String, Vec<u8>)> = images.iter().enumerate().map(|(i, b)| (format!("Image {}", i + 1), b.to_vec())).collect();
        let Some((pw, ph)) = page_size(size.unwrap_or("fit"))? else {
            let doc = pdfcraft_create::from_images(&named).map_err(|e| e.to_string())?;
            return save_new(&doc);
        };
        let mut doc = Document::new_empty();
        for (i, (name, bytes)) in named.iter().enumerate() {
            let (xobj, (iw, ih)) = pdfcraft_create::image_xobject(&mut doc, name, bytes).map_err(|e| e.to_string())?;
            let (w, h) = if iw > ih { (ph, pw) } else { (pw, ph) };
            pdfcraft_organize::insert_blank_page(&mut doc, i, w, h).map_err(|e| e.to_string())?;
            let margin = 36.0;
            let (bw, bh) = (w - 2.0 * margin, h - 2.0 * margin);
            let k = if iw > 0.0 && ih > 0.0 { (bw / iw).min(bh / ih) } else { 1.0 };
            let (dw, dh) = (iw * k, ih * k);
            if !(dw.is_finite() && dh.is_finite() && dw > 0.0 && dh > 0.0) {
                return Err(format!("{name} has no usable size"));
            }
            let (x, y) = ((w - dw) / 2.0, (h - dh) / 2.0);
            let img = AddedImage::new([x, y, x + dw, y + dh], xobj);
            pdfcraft_edit::add_content(&mut doc, i, &Content::Image(img)).map_err(|e| e.to_string())?;
        }
        save_new(&doc)
    })
}

/// Reduce file size (images above 225 ppi to 150 ppi, medium JPEG; duplicates merged; object
/// streams). Returns the original bytes when the result would not be smaller.
pub fn compress(bytes: &[u8]) -> Result<Vec<u8>, String> {
    guard(|| {
        let mut doc = open(bytes)?;
        pdfcraft_optimize::optimize(&mut doc, &pdfcraft_optimize::Settings::default()).map_err(|e| e.to_string())?;
        let all: Vec<pdfcraft_cos::ObjRef> = doc.object_numbers().into_iter().map(|n| pdfcraft_cos::ObjRef::new(n, doc.generation(n))).collect();
        pdfcraft_organize::dedupe_resources(&mut doc, &all, false);
        let out = save_new(&doc)?;
        Ok(if out.len() < bytes.len() { out } else { bytes.to_vec() })
    })
}

// ── annotations ──

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|f| f.is_finite())
}

fn color(v: &Value) -> Result<Rgb, String> {
    let Some(c) = v.get("color") else { return Ok([0.86, 0.15, 0.15]) };
    if let Some(a) = c.as_array() {
        let ch: Vec<f64> = a.iter().filter_map(Value::as_f64).filter(|f| f.is_finite()).collect();
        return match ch.as_slice() {
            [r, g, b] => Ok([r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]),
            _ => Err("color must be \"#rrggbb\" or [r, g, b] in 0..1".into()),
        };
    }
    let s = c.as_str().ok_or("color must be \"#rrggbb\" or [r, g, b] in 0..1")?;
    let hex = s.trim().trim_start_matches('#');
    let full: String = match hex.len() {
        3 => hex.chars().flat_map(|ch| [ch, ch]).collect(),
        6 => hex.to_string(),
        _ => return Err(format!("bad colour `{s}`")),
    };
    let byte = |i: usize| full.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).map(|b| f64::from(b) / 255.0);
    match (byte(0), byte(2), byte(4)) {
        (Some(r), Some(g), Some(b)) => Ok([r, g, b]),
        _ => Err(format!("bad colour `{s}`")),
    }
}

/// A normalised page point (0..1 from the top-left of the page as displayed) in user space.
struct PageMap {
    size: (f64, f64),
}

impl PageMap {
    fn new(doc: &Document, page: usize) -> Result<Self, String> {
        let p = pdfcraft_model::pages(doc).get(page).cloned().ok_or_else(|| format!("page {page} does not exist"))?;
        Ok(PageMap { size: p.display_size(doc) })
    }

    fn user(&self, doc: &Document, page: usize, v: &Value) -> Result<[f64; 2], String> {
        let a = v.as_array().ok_or("a point must be [x, y]")?;
        let (Some(x), Some(y)) = (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) else {
            return Err("a point must be [x, y]".into());
        };
        if !(x.is_finite() && y.is_finite()) {
            return Err("a point must be finite".into());
        }
        let (x, y) = (x.clamp(-1.0, 2.0), y.clamp(-1.0, 2.0));
        pdfcraft_measure::view_to_user(doc, page, [x * self.size.0, y * self.size.1]).map_err(|e| e.to_string())
    }
}

fn rect_of(a: [f64; 2], b: [f64; 2]) -> [f64; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
}

/// Parse one annotation item into a [`NewAnnotation`]; `points` counts stroke points so far.
fn item(doc: &Document, v: &Value, points: &mut usize) -> Result<NewAnnotation, String> {
    let page = v.get("page").and_then(Value::as_u64).ok_or("each item needs a page (0-based)")?;
    let page = page_index(doc, u32::try_from(page).map_err(|_| "page out of range")?)?;
    let map = PageMap::new(doc, page)?;
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("ink");
    let shape = match kind {
        "ink" => {
            let strokes = v.get("strokes").and_then(Value::as_array).ok_or("an ink item needs strokes")?;
            let mut out = Vec::new();
            for s in strokes {
                let pts = s.as_array().ok_or("a stroke must be a list of [x, y]")?;
                *points = points.saturating_add(pts.len());
                if *points > MAX_POINTS {
                    return Err(format!("more than {MAX_POINTS} points"));
                }
                let stroke = pts.iter().map(|p| map.user(doc, page, p)).collect::<Result<Vec<_>, _>>()?;
                if !stroke.is_empty() {
                    out.push(stroke);
                }
            }
            if out.is_empty() {
                return Err("an ink item has no points".into());
            }
            Shape::Ink { strokes: out }
        }
        "rect" => {
            let a = map.user(doc, page, v.get("from").ok_or("a rect needs from and to")?)?;
            let b = map.user(doc, page, v.get("to").ok_or("a rect needs from and to")?)?;
            Shape::Rectangle { rect: rect_of(a, b) }
        }
        "line" | "arrow" => {
            let from = map.user(doc, page, v.get("from").ok_or("a line needs from and to")?)?;
            let to = map.user(doc, page, v.get("to").ok_or("a line needs from and to")?)?;
            let end = if kind == "arrow" { LineEnding::OpenArrow } else { LineEnding::None };
            Shape::Line { from, to, start: LineEnding::None, end }
        }
        other => return Err(format!("unknown item type `{other}` (ink, rect, line or arrow)")),
    };
    let style = Style {
        color: color(v)?,
        opacity: num(v, "opacity").unwrap_or(1.0).clamp(0.0, 1.0),
        width: num(v, "width").unwrap_or(2.0).clamp(0.0, 100.0),
        fill: None,
    };
    let author = v.get("author").and_then(Value::as_str).unwrap_or("").chars().take(200).collect();
    Ok(NewAnnotation { page, shape, style, contents: String::new(), author })
}

/// Add ink and markup annotations. `json` is either a list of items or
/// `{"items": [...], "flatten": bool}`. Items:
/// - `{"page", "strokes": [[[x, y], ...], ...], "color", "width", "opacity"}` (ink; `type` may be
///   omitted or `"ink"`),
/// - `{"page", "type": "rect" | "line" | "arrow", "from": [x, y], "to": [x, y], ...style}`.
///
/// Points are normalised 0..1 from the top-left of the page as displayed; `width` is in points;
/// `color` is `"#rrggbb"` or `[r, g, b]` in 0..1; pages are 0-based. With `flatten`, the
/// annotations are drawn into the page content. `now_ms` is the caller's `Date.now()`.
pub fn add_ink(bytes: &[u8], json: &str, now_ms: f64) -> Result<Vec<u8>, String> {
    guard(|| {
        check_size(json.len(), "the annotation list")?;
        let v: Value = serde_json::from_str(json).map_err(|e| format!("annotations: {e}"))?;
        let (items, flatten) = match &v {
            Value::Array(a) => (a.as_slice(), false),
            Value::Object(o) => (
                o.get("items").and_then(Value::as_array).map(Vec::as_slice).ok_or("expected {\"items\": [...]}")?,
                o.get("flatten").and_then(Value::as_bool).unwrap_or(false),
            ),
            _ => return Err("expected a list of annotations".into()),
        };
        if items.len() > MAX_ITEMS {
            return Err(format!("more than {MAX_ITEMS} annotations"));
        }
        let mut doc = open(bytes)?;
        let stamp = date(now_ms);
        let base = if now_ms.is_finite() && now_ms > 0.0 { now_ms as u64 } else { 0 };
        let mut points = 0usize;
        let mut pages = Vec::new();
        for (i, v) in items.iter().enumerate() {
            let new = item(&doc, v, &mut points)?;
            pages.push(new.page);
            let meta = Meta { date: stamp.clone(), id: format!("pdfcraft-wasm-{base:x}-{i}") };
            pdfcraft_annot::add_annotation(&mut doc, &new, &meta).map_err(|e| e.to_string())?;
        }
        if flatten && !pages.is_empty() {
            pages.sort_unstable();
            pages.dedup();
            pdfcraft_edit::flatten(&mut doc, &pages, true, false).map_err(|e| e.to_string())?;
        }
        save_edited(&doc, now_ms)
    })
}

/// Draw an image (normally a transparent PNG) over the whole of `page` (0-based), stretched to
/// the page as displayed. The original page content is kept underneath.
pub fn add_page_image(bytes: &[u8], page: u32, image: &[u8], now_ms: f64) -> Result<Vec<u8>, String> {
    guard(|| {
        check_size(image.len(), "the image")?;
        let mut doc = open(bytes)?;
        let page = page_index(&doc, page)?;
        let (w, h) = PageMap::new(&doc, page)?.size;
        let (xobj, _) = pdfcraft_create::image_xobject(&mut doc, "overlay", image).map_err(|e| e.to_string())?;
        pdfcraft_edit::add_content(&mut doc, page, &Content::Image(AddedImage::new([0.0, 0.0, w, h], xobj))).map_err(|e| e.to_string())?;
        save_edited(&doc, now_ms)
    })
}

// ── JavaScript bindings ──

fn js(e: String) -> JsError {
    JsError::new(&e)
}

fn byte_list(list: &js_sys::Array) -> Result<Vec<Vec<u8>>, JsError> {
    let mut out = Vec::new();
    let mut total = 0usize;
    for v in list.iter() {
        let a = v.dyn_into::<js_sys::Uint8Array>().map_err(|_| JsError::new("expected a list of Uint8Array"))?;
        let len = a.length() as usize;
        total = total.saturating_add(len);
        if total > MAX_INPUT {
            return Err(JsError::new("the input is larger than 200 MB"));
        }
        out.push(a.to_vec());
    }
    Ok(out)
}

/// The library version.
#[wasm_bindgen]
pub fn version() -> String {
    library_version().into()
}

/// The number of pages of a PDF.
#[wasm_bindgen(js_name = pageCount)]
pub fn page_count_js(bytes: &[u8]) -> Result<u32, JsError> {
    page_count(bytes).map_err(js)
}

/// Combine PDFs (a list of `Uint8Array`), in order.
#[wasm_bindgen(js_name = mergePdfs)]
pub fn merge_js(list: js_sys::Array) -> Result<Vec<u8>, JsError> {
    let parts = byte_list(&list)?;
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    merge(&refs).map_err(js)
}

/// A new PDF of the chosen pages (0-based).
#[wasm_bindgen(js_name = extractPages)]
pub fn extract_js(bytes: &[u8], pages: &[u32]) -> Result<Vec<u8>, JsError> {
    extract(bytes, pages).map_err(js)
}

/// One page per image; `pageSize` is `"fit"` (default), `"a4"` or `"letter"`.
#[wasm_bindgen(js_name = imagesToPdf)]
pub fn images_to_pdf_js(list: js_sys::Array, page_size: Option<String>) -> Result<Vec<u8>, JsError> {
    let parts = byte_list(&list)?;
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    images_to_pdf(&refs, page_size.as_deref()).map_err(js)
}

/// Reduce file size; returns the input when that would not be smaller.
#[wasm_bindgen(js_name = compress)]
pub fn compress_js(bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    compress(bytes).map_err(js)
}

/// Add ink/rect/line/arrow annotations from JSON (see the crate docs); `nowMs` is `Date.now()`.
#[wasm_bindgen(js_name = addInk)]
pub fn add_ink_js(bytes: &[u8], json: &str, now_ms: f64) -> Result<Vec<u8>, JsError> {
    add_ink(bytes, json, now_ms).map_err(js)
}

/// Overlay an image (transparent PNG) over a whole page (0-based); `nowMs` is `Date.now()`.
#[wasm_bindgen(js_name = addPageImage)]
pub fn add_page_image_js(bytes: &[u8], page: u32, png: &[u8], now_ms: f64) -> Result<Vec<u8>, JsError> {
    add_page_image(bytes, page, png, now_ms).map_err(js)
}

#[cfg(test)]
mod tests;
