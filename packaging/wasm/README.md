# @tutorfulhk/pdfcraft-wasm

PdfCraft's PDF byte jobs as a lean, headless WebAssembly library (`crates/wasm`). It links the
object layer and the organise, annotation, edit, create and optimise crates only — not the
engine, so no JavaScript interpreter, XFA, OCR or signing. Run it in a Web Worker.

```js
import init, * as pdf from "@tutorfulhk/pdfcraft-wasm";
await init();
pdf.version();                                  // "0.5.0"
pdf.pageCount(bytes);                           // number
pdf.mergePdfs([a, b, c]);                       // Uint8Array
pdf.extractPages(bytes, new Uint32Array([2, 0])); // pages are 0-based
pdf.imagesToPdf([png, jpeg], "a4");             // "fit" (default), "a4" or "letter"
pdf.compress(bytes);                            // never larger than the input
pdf.addInk(bytes, JSON.stringify({ items, flatten: false }), Date.now());
pdf.addPageImage(bytes, page, transparentPng, Date.now());
```

`addInk` items: `{page, strokes: [[[x, y], ...], ...], color, width, opacity}` for ink, or
`{page, type: "rect" | "line" | "arrow", from: [x, y], to: [x, y], color, width, opacity}`.
Points are normalised 0..1 from the top-left of the page as displayed; `width` is in points;
`color` is `"#rrggbb"` or `[r, g, b]` in 0..1. Annotation text is not supported (the built-in
fonts are Latin only): rasterise text and use `addPageImage`.

Inputs are capped at 200 MB. Errors throw; the module never panics on bad input (a trap means a
bug — restart the worker). Build: `packaging/wasm/build.sh [OUT_DIR]`.
