//! Fill needs closed outlines: what a Fill layer makes of closed, open and holed shapes, and what
//! it says about the shapes it can't fill.

use bc_doc::{Document, LayerMode, ShapeKind};
use bc_geom::{Affine, BezPath, Point, polyline_path};

fn ring(x0: f64, y0: f64, x1: f64, y1: f64) -> BezPath {
    polyline_path(&[Point::new(x0, y0), Point::new(x1, y0), Point::new(x1, y1), Point::new(x0, y1)], true)
}

fn lines(doc: &mut Document, corners: &[(f64, f64)]) {
    for w in corners.windows(2) {
        let pts = [Point::new(w[0].0, w[0].1), Point::new(w[1].0, w[1].1)];
        doc.add_shape(ShapeKind::Path(polyline_path(&pts, false)), Affine::IDENTITY, 0);
    }
}

fn paths(doc: &Document) -> usize {
    bc_job::build(doc, None, bc_job::BuildOptions::default()).layers.iter().map(|l| l.paths.len()).sum()
}

#[test]
fn a_closed_shape_on_a_fill_layer_is_scanned() {
    let mut doc = Document::default();
    doc.add_shape(ShapeKind::Path(ring(10.0, 10.0, 110.0, 60.0)), Affine::IDENTITY, 0);
    doc.layers[0].mode = LayerMode::Fill;
    // 100 × 50 mm at a 0.1 mm interval: a scanline every 0.1 mm.
    assert_eq!(paths(&doc), 500);
}

#[test]
fn a_hole_in_a_closed_shape_stays_empty() {
    let mut path = ring(10.0, 10.0, 110.0, 60.0);
    path.extend(ring(40.0, 25.0, 80.0, 45.0).elements().iter().copied());
    let mut doc = Document::default();
    doc.add_shape(ShapeKind::Path(path), Affine::IDENTITY, 0);
    doc.layers[0].mode = LayerMode::Fill;
    let job = bc_job::build(&doc, None, bc_job::BuildOptions::default());
    // Group the scanlines by row (0.1 mm), left to right whatever way each runs.
    let mut rows: std::collections::BTreeMap<i64, Vec<(f64, f64)>> = std::collections::BTreeMap::new();
    for p in &job.layers[0].paths {
        let y = (p.points[0].y * 10.0).round() as i64;
        rows.entry(y).or_default().push((p.points[0].x.min(p.points[1].x), p.points[0].x.max(p.points[1].x)));
    }
    let mut through = rows.iter().find(|(y, _)| (**y as f64 / 10.0 - 35.0).abs() < 0.05).map(|(_, spans)| spans.clone()).expect("a row through the hole");
    through.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(through.len(), 2, "a row across the hole has one span each side: {through:?}");
    assert!((through[0].0 - 10.0).abs() < 1e-6 && (through[0].1 - 40.0).abs() < 1e-6, "left of the hole: {through:?}");
    assert!((through[1].0 - 80.0).abs() < 1e-6 && (through[1].1 - 110.0).abs() < 1e-6, "right of the hole: {through:?}");
}

#[test]
fn open_lines_on_a_fill_layer_are_reported_not_silently_dropped() {
    // A DXF-style outline of separate lines: there is no inside to fill.
    let mut doc = Document::default();
    lines(&mut doc, &[(20.0, 20.0), (120.0, 20.0), (120.0, 90.0), (20.0, 90.0), (20.0, 20.0)]);
    doc.layers[0].mode = LayerMode::Fill;
    let job = bc_job::build(&doc, None, bc_job::BuildOptions::default());
    assert!(job.layers.is_empty(), "nothing can be filled");
    let note = job.skipped.iter().find(|(i, _)| *i == 0).map(|(_, w)| w.clone()).expect("the layer says why");
    assert!(note.contains("4 open shape(s)"), "{note}");
    assert!(note.contains("no inside to fill"), "{note}");
    assert!(note.contains("Auto-Join"), "it says how to fix it: {note}");
}

#[test]
fn open_lines_on_a_fill_and_line_layer_still_cut_their_outlines() {
    let mut doc = Document::default();
    lines(&mut doc, &[(20.0, 20.0), (120.0, 20.0), (120.0, 90.0), (20.0, 90.0), (20.0, 20.0)]);
    doc.layers[0].mode = LayerMode::FillThenLine;
    let job = bc_job::build(&doc, None, bc_job::BuildOptions::default());
    assert!(!job.layers.is_empty(), "the lines are still cut");
    let note = job.skipped.iter().find(|(i, _)| *i == 0).map(|(_, w)| w.clone()).expect("the layer says why there is no fill");
    assert!(note.contains("only their outlines are cut"), "{note}");
}

#[test]
fn a_layer_with_nothing_closed_but_a_qr_code_fills_the_code_only() {
    // The mix from a real design: a QR code (closed modules) and an open outline on one layer.
    let mut doc = Document::default();
    doc.add_shape(ShapeKind::Path(ring(10.0, 10.0, 30.0, 30.0)), Affine::IDENTITY, 0);
    lines(&mut doc, &[(50.0, 10.0), (90.0, 10.0), (90.0, 40.0)]);
    doc.layers[0].mode = LayerMode::Fill;
    let job = bc_job::build(&doc, None, bc_job::BuildOptions::default());
    assert!(!job.layers.is_empty(), "the closed shape still fills");
    assert!(job.skipped.iter().any(|(i, w)| *i == 0 && w.contains("2 open shape(s)")), "{:?}", job.skipped);
}
