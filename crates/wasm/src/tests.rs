use super::*;

/// A blank PDF of `n` Letter pages, built in code.
fn blank(n: usize) -> Vec<u8> {
    save_new(&pdfcraft_create::blank(612.0, 792.0, n).unwrap()).unwrap()
}

/// A `w × h` RGBA PNG, half transparent red.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().unwrap();
        let data: Vec<u8> = (0..w * h).flat_map(|_| [255, 0, 0, 128]).collect();
        wr.write_image_data(&data).unwrap();
    }
    out
}

fn annots(bytes: &[u8], page: usize) -> Vec<String> {
    let doc = open(bytes).unwrap();
    let refs = pdfcraft_annot::page_refs(&doc).unwrap();
    let p = doc.get(refs[page]);
    let Some(list) = p.as_dict().and_then(|d| d.get(b"Annots")).map(|o| doc.resolve(o).clone()) else { return vec![] };
    list.as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let d = doc.resolve(o);
            let st = d.as_dict().unwrap().get(b"Subtype").unwrap();
            String::from_utf8_lossy(st.as_name().unwrap()).into_owned()
        })
        .collect()
}

#[test]
fn version_and_count() {
    assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    assert_eq!(page_count(&blank(3)).unwrap(), 3);
}

#[test]
fn merge_and_extract() {
    let m = merge(&[&blank(2), &blank(3)]).unwrap();
    assert!(m.starts_with(b"%PDF"));
    assert_eq!(page_count(&m).unwrap(), 5);
    let e = extract(&m, &[4, 0, 0]).unwrap();
    assert_eq!(page_count(&e).unwrap(), 3);
    assert!(extract(&m, &[5]).is_err());
    assert!(extract(&m, &[]).is_err());
    assert!(merge(&[]).is_err());
}

#[test]
fn images_fit_and_a4() {
    let a = png(40, 20);
    let b = png(10, 30);
    let fit = images_to_pdf(&[&a, &b], None).unwrap();
    assert_eq!(page_count(&fit).unwrap(), 2);
    let a4 = images_to_pdf(&[&a, &b], Some("a4")).unwrap();
    let doc = open(&a4).unwrap();
    let pages = pdfcraft_model::pages(&doc);
    assert_eq!(pages.len(), 2);
    let (w0, h0) = pages[0].display_size(&doc);
    let (w1, h1) = pages[1].display_size(&doc);
    assert!(w0 > h0, "wide image gets a landscape page");
    assert!(h1 > w1);
    assert!(images_to_pdf(&[&a], Some("tabloid")).is_err());
    assert!(images_to_pdf(&[b"not an image"], None).is_err());
}

#[test]
fn compress_never_grows() {
    let src = blank(2);
    let out = compress(&src).unwrap();
    assert!(out.len() <= src.len());
    assert_eq!(page_count(&out).unwrap(), 2);
}

#[test]
fn ink_and_markup() {
    let src = blank(2);
    let json = r##"[
        {"page": 0, "strokes": [[[0.1, 0.1], [0.2, 0.25], [0.3, 0.2]]], "color": "#2255ff", "width": 3, "opacity": 0.5},
        {"page": 1, "type": "rect", "from": [0.1, 0.1], "to": [0.5, 0.4], "color": [0, 1, 0]},
        {"page": 1, "type": "arrow", "from": [0.1, 0.9], "to": [0.8, 0.6]},
        {"page": 1, "type": "line", "from": [0.2, 0.2], "to": [0.3, 0.3], "color": "#f00"}
    ]"##;
    let out = add_ink(&src, json, 1_760_000_000_000.0).unwrap();
    assert!(out.starts_with(&src[..20]), "incremental save keeps the original");
    assert_eq!(annots(&out, 0), ["Ink"]);
    assert_eq!(annots(&out, 1), ["Square", "Line", "Line"]);
    // Normalised top-left becomes user-space top-left: y near the page top.
    let doc = open(&out).unwrap();
    let refs = pdfcraft_annot::page_refs(&doc).unwrap();
    let p = doc.get(refs[0]);
    let a = doc.resolve(&p.as_dict().unwrap().get(b"Annots").unwrap().clone()).clone();
    let ink = doc.resolve(&a.as_array().unwrap()[0]).clone();
    let rect: Vec<f64> = ink.as_dict().unwrap().get(b"Rect").unwrap().as_array().unwrap().iter().filter_map(|o| o.as_f64()).collect();
    assert!(rect[3] > 792.0 * 0.7, "{rect:?}");

    let flat = add_ink(&src, &format!("{{\"items\": {json}, \"flatten\": true}}"), 0.0).unwrap();
    assert!(annots(&flat, 1).is_empty());
}

#[test]
fn page_overlay() {
    let src = blank(2);
    let out = add_page_image(&src, 1, &png(64, 80), 1_760_000_000_000.0).unwrap();
    assert_eq!(page_count(&out).unwrap(), 2);
    assert!(out.len() > src.len());
    assert!(add_page_image(&src, 2, &png(4, 4), 0.0).is_err());
    assert!(add_page_image(&src, 0, b"junk", 0.0).is_err());
}

#[test]
fn junk_never_panics() {
    let src = blank(1);
    let junk: [&[u8]; 4] = [b"", b"%PDF-1.7\n", b"garbage", &src[..src.len() / 2]];
    for j in junk {
        let _ = page_count(j);
        let _ = merge(&[j, &src]);
        let _ = extract(j, &[0]);
        let _ = compress(j);
        let _ = add_ink(j, "[]", 0.0);
        let _ = add_page_image(j, 0, &png(2, 2), 0.0);
        let _ = images_to_pdf(&[j], Some("a4"));
    }
    for json in [
        "",
        "{",
        "null",
        "7",
        "{}",
        "[1]",
        "[{}]",
        r#"[{"page": 99, "strokes": [[[0,0]]]}]"#,
        r#"[{"page": 0, "strokes": [[["a", 1]]]}]"#,
        r#"[{"page": 0, "type": "blob"}]"#,
        r#"[{"page": 0, "type": "rect"}]"#,
        r##"[{"page": 0, "strokes": [[[0,0]]], "color": "#zzz"}]"##,
        r#"[{"page": -1, "strokes": [[[0,0]]]}]"#,
        r#"[{"page": 0, "strokes": [[[0,0]]], "color": [1]}]"#,
    ] {
        assert!(add_ink(&src, json, f64::NAN).is_err(), "{json}");
    }
    // Out-of-page points are clamped near the page, not rejected.
    assert!(add_ink(&src, r#"[{"page": 0, "strokes": [[[1e308, -1e308]]]}]"#, 0.0).is_ok());
}
