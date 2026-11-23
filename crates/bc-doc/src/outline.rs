//! Outlines that don't close: finding the exact spot that stops a design being cut as one
//! outline, and repairing it in one step.
//!
//! A DXF (or SVG) outline exported as separate lines, a polyline whose ends meet but which was
//! never flagged closed, and a single open line all look like an outline on the canvas until
//! something needs one closed shape. This module pulls the open subpaths apart, works out which of
//! them meet end to end, and reports each broken outline with the spot to show the user.
//!
//! It also finds the run of shapes joined end to end with a given shape, which is what a
//! double-click on the canvas selects (see [`connected`]).

use crate::{Document, LayerMode, Shape, ShapeKind};
use bc_geom::{Point, Polyline, Rect};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Ends closer together than this (mm) count as joined: the same tolerance Auto-Join uses.
pub const JOIN_TOL: f64 = 0.05;
/// Paths are flattened this finely (mm) for touch tests, well under the join tolerance so a
/// curve's outline is compared where it really runs.
const TOUCH_FLATNESS: f64 = 0.05;
/// Paths are flattened this finely (mm) when reporting outlines: only the ends and the overall
/// shape matter there, and coarse flattening keeps a busy drawing quick.
const REPORT_FLATNESS: f64 = 0.2;
/// Grid cell (mm) for the connection test.
const CELL: f64 = 5.0;
/// Stop adding shapes to a double-click run past this many segments, so a very busy drawing still
/// answers at once. The run may then be incomplete, but only on drawings this large.
const MAX_RUN_SEGMENTS: usize = 150_000;
/// Most open subpaths analysed at once; past this, a drawing is too busy to report one outline at
/// a time usefully and the rest are left alone.
const MAX_OPEN_PATHS: usize = 20_000;

/// Something that stops a group of paths being one closed outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    /// The ends don't meet: an open line, or a gap where an outline should close.
    OpenEnds,
    /// The ends meet, but the outline is still open (separate lines, or never flagged closed).
    NotClosed,
    /// A closed outline with too few points or no area.
    Degenerate,
}

impl IssueKind {
    pub fn label(self) -> &'static str {
        match self {
            IssueKind::OpenEnds => "open outline",
            IssueKind::NotClosed => "outline not closed",
            IssueKind::Degenerate => "empty outline",
        }
    }
}

/// A repair that can be applied without guessing at what the user meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Join separate lines that already meet end to end into one outline.
    Join { lines: usize },
    /// Close one shape whose two ends already meet.
    Close,
    /// Close one open shape with a straight line across its gap.
    CloseGap,
}

impl Fix {
    /// The button label for this repair.
    pub fn label(self) -> String {
        match self {
            Fix::Join { lines } => format!("Join {lines} lines"),
            Fix::Close => "Close outline".into(),
            Fix::CloseGap => "Close gap".into(),
        }
    }
}

/// One outline that won't close, with where it is and how to repair it.
#[derive(Clone, Debug)]
pub struct Issue {
    /// Stays the same while the same violation remains, so the canvas can follow it through edits.
    pub key: String,
    pub kind: IssueKind,
    /// One line, for the warning list.
    pub hint: String,
    /// What is wrong, and how to repair it.
    pub message: String,
    /// The shapes involved.
    pub shapes: BTreeSet<u64>,
    /// The layer the outline starts on: where the Layers panel shows its warning.
    pub layer: usize,
    /// The area to frame in the canvas.
    pub focus: Rect,
    /// The exact spot to mark, when there is one.
    pub marker: Option<Point>,
    /// The two ends of a gap, drawn as a dashed line.
    pub gap: Option<(Point, Point)>,
    /// The outline itself, drawn over the design in the problem's colour.
    pub highlights: Vec<Polyline>,
    /// The repair offered next to the warning, when one is safe to apply.
    pub fix: Option<Fix>,
    /// The outline is on a Fill layer, where an open shape leaves no inside to fill.
    pub fills: bool,
}

impl Issue {
    /// The repair button's label, when this problem has a safe one-click fix.
    pub fn fix_label(&self) -> Option<String> {
        self.fix.map(Fix::label)
    }
}

/// An open subpath found in a shape, with where it sits in the drawing.
struct OpenPath {
    shape: u64,
    layer: usize,
    shape_order: usize,
    subpath: usize,
    points: Vec<Point>,
}

fn bounds_of(points: impl Iterator<Item = Point>) -> Option<Rect> {
    let mut out: Option<Rect> = None;
    for p in points {
        let r = Rect::new(p.x, p.y, p.x, p.y);
        out = Some(out.map_or(r, |q| q.union(r)));
    }
    out
}

fn mid(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}

/// Twice the signed area of a polygon (positive for one winding, negative for the other).
fn double_area(points: &[Point]) -> f64 {
    let mut sum = 0.0;
    for i in 0..points.len() {
        let (a, b) = (points[i], points[(i + 1) % points.len()]);
        sum += a.x * b.y - b.x * a.y;
    }
    sum
}

/// The line segments of a shape's outline in document coordinates, including the closing edge of
/// each closed subpath.
fn shape_segments(path: &bc_geom::BezPath) -> Vec<(Point, Point)> {
    let mut out = Vec::new();
    for line in bc_geom::flatten(path, TOUCH_FLATNESS) {
        let mut pts = line.points;
        if line.closed && let Some(&first) = pts.first() {
            pts.push(first);
        }
        for w in pts.windows(2) {
            if w[0].distance(w[1]) > 1e-9 {
                out.push((w[0], w[1]));
            }
        }
    }
    out
}

/// Whether two straight segments touch: either end of one lies on the other, or they cross. The
/// point test covers T-junctions and shared ends, so the crossing test needn't.
fn segments_touch(a: Point, b: Point, c: Point, d: Point, tol: f64) -> bool {
    if bc_geom::distance_to_segment(a, c, d) <= tol
        || bc_geom::distance_to_segment(b, c, d) <= tol
        || bc_geom::distance_to_segment(c, a, b) <= tol
        || bc_geom::distance_to_segment(d, a, b) <= tol
    {
        return true;
    }
    let turn = |p: Point, q: Point, r: Point| (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
    let (o1, o2, o3, o4) = (turn(a, b, c), turn(a, b, d), turn(c, d, a), turn(c, d, b));
    (o1 > 0.0) != (o2 > 0.0) && (o3 > 0.0) != (o4 > 0.0)
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[rb] = ra;
    }
}

/// The shapes joined end to end with `seed`: touching at a vertex, meeting at a T-junction, or
/// crossing. Locked shapes stay out of the run, as do shapes on a different kind of layer (a
/// dimension or jig slot on a tool layer never joins a design outline, and the other way round).
/// Returns the seed alone when it isn't there or nothing touches it.
pub fn connected(doc: &Document, seed: u64, tol: f64) -> BTreeSet<u64> {
    let mut out = BTreeSet::from([seed]);
    let Some(seed_shape) = doc.shape(seed) else { return BTreeSet::new() };
    if seed_shape.locked || !doc.layers.get(seed_shape.layer).is_some_and(|l| l.visible) {
        return out;
    }
    let on_tool_layer = crate::is_tool_layer(seed_shape.layer);
    let mut ids: Vec<u64> = Vec::new();
    let mut segments: Vec<(usize, Point, Point)> = Vec::new();
    for s in &doc.shapes {
        // Pictures are a frame around pixels, not art to join up.
        if s.locked || matches!(s.kind, ShapeKind::Image { .. }) {
            continue;
        }
        if !doc.layers.get(s.layer).is_some_and(|l| l.visible) || crate::is_tool_layer(s.layer) != on_tool_layer {
            continue;
        }
        let index = ids.len();
        segments.extend(shape_segments(&s.world_path()).into_iter().map(|(a, b)| (index, a, b)));
        ids.push(s.id);
        if segments.len() >= MAX_RUN_SEGMENTS {
            break;
        }
    }
    let Some(seed_index) = ids.iter().position(|id| *id == seed) else { return out };
    // Segments in a 5 mm grid, so only neighbours are compared.
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, (_, a, b)) in segments.iter().enumerate() {
        let (x0, x1) = ((a.x.min(b.x) / CELL).floor() as i64, (a.x.max(b.x) / CELL).floor() as i64);
        let (y0, y1) = ((a.y.min(b.y) / CELL).floor() as i64, (a.y.max(b.y) / CELL).floor() as i64);
        for x in x0..=x1 {
            for y in y0..=y1 {
                grid.entry((x, y)).or_default().push(i);
            }
        }
    }
    let mut parent: Vec<usize> = (0..ids.len()).collect();
    let mut tested: HashSet<(usize, usize)> = HashSet::new();
    for cell in grid.values() {
        for (k, &i) in cell.iter().enumerate() {
            for &j in &cell[k + 1..] {
                let (si, ai, bi) = segments[i];
                let (sj, aj, bj) = segments[j];
                if si == sj || find(&mut parent, si) == find(&mut parent, sj) {
                    continue;
                }
                // Most segments that share a cell are still too far apart to touch, and a pair
                // that shares several cells need only be looked at once.
                if ai.x.min(bi.x) - tol > aj.x.max(bj.x)
                    || aj.x.min(bj.x) - tol > ai.x.max(bi.x)
                    || ai.y.min(bi.y) - tol > aj.y.max(bj.y)
                    || aj.y.min(bj.y) - tol > ai.y.max(bi.y)
                    || !tested.insert((i.min(j), i.max(j)))
                {
                    continue;
                }
                if segments_touch(ai, bi, aj, bj, tol) {
                    union(&mut parent, si, sj);
                }
            }
        }
    }
    let root = find(&mut parent, seed_index);
    out.extend(ids.iter().enumerate().filter(|(i, _)| find(&mut parent, *i) == root).map(|(_, id)| *id));
    out
}

/// Every broken outline in the design: open runs of lines and curves, ends that meet but aren't
/// joined or closed, and closed outlines with no area.
///
/// Only shapes that will be sent to the laser are checked (tool layers and layers with Output off
/// are left alone), and the issues come back in drawing order.
pub fn issues(doc: &Document) -> Vec<Issue> {
    let mut found: Vec<((usize, usize), Issue)> = Vec::new();
    let mut open: Vec<OpenPath> = Vec::new();
    // Text and barcodes come from a font or an encoder: single-line fonts are open strokes by
    // design, so they are not outlines that failed to close. Pictures are a frame around pixels.
    let checked = doc
        .shapes
        .iter()
        .filter(|s| !crate::is_tool_layer(s.layer))
        .filter(|s| !matches!(s.kind, ShapeKind::Image { .. } | ShapeKind::Text { .. } | ShapeKind::Barcode { .. }))
        .filter(|s| doc.layers.get(s.layer).is_some_and(|l| l.output));
    for (shape_order, s) in checked.enumerate() {
        for (subpath, line) in bc_geom::flatten(&s.world_path(), REPORT_FLATNESS).into_iter().enumerate() {
            if line.closed {
                let hint = if line.points.len() < 3 {
                    Some("A closed outline needs at least 3 points")
                } else if double_area(&line.points).abs() < 2e-6 {
                    Some("This closed outline has no area")
                } else {
                    None
                };
                if let Some(hint) = hint {
                    found.push(((shape_order, subpath), degenerate(doc, s, subpath, &line, hint)));
                }
            } else if line.points.len() >= 2 && open.len() < MAX_OPEN_PATHS {
                open.push(OpenPath { shape: s.id, layer: s.layer, shape_order, subpath, points: line.points });
            }
        }
    }
    found.extend(chain_issues(doc, &open));
    found.sort_by_key(|(order, _)| *order);
    found.into_iter().map(|(_, issue)| issue).filter(|issue| !is_bare_line(issue)).collect()
}

/// A lone straight segment on a cutting layer: a cut or score line, not an outline that failed to
/// close. (On a Fill layer it is reported, because there is nothing to fill; and a segment that
/// helps make a bigger outline is reported with the rest of it.)
fn is_bare_line(issue: &Issue) -> bool {
    issue.kind == IssueKind::OpenEnds && !issue.fills && issue.fix.is_none() && issue.shapes.len() == 1 && issue.highlights.len() == 1 && issue.highlights[0].points.len() == 2
}

fn degenerate(doc: &Document, shape: &Shape, subpath: usize, line: &Polyline, hint: &str) -> Issue {
    let bb = bounds_of(line.points.iter().copied()).unwrap_or(Rect::ZERO);
    let fills = doc.layers.get(shape.layer).is_some_and(|l| l.mode == LayerMode::Fill);
    Issue {
        key: format!("degenerate:{}:{subpath}", shape.id),
        kind: IssueKind::Degenerate,
        hint: hint.into(),
        message: with_fill(fills, format!("This outline cannot be cut as a closed shape: {}. Remove it, or draw it again.", hint.to_lowercase())),
        shapes: BTreeSet::from([shape.id]),
        layer: shape.layer,
        focus: bb.inflate(2.0, 2.0),
        marker: Some(bb.center()),
        gap: None,
        highlights: vec![line.clone()],
        fix: None,
        fills,
    }
}

/// Prefixes a message when the outline is on a Fill layer, where an open shape means no fill.
fn with_fill(fills: bool, message: String) -> String {
    if fills {
        format!("This outline is on a Fill layer, so there is no inside to fill. {message}")
    } else {
        message
    }
}

/// One issue per group of open paths that meet, in drawing order.
fn chain_issues(doc: &Document, open: &[OpenPath]) -> Vec<((usize, usize), Issue)> {
    if open.is_empty() {
        return Vec::new();
    }
    // Join path ends that touch into little graphs, then report one issue per group.
    let tol = JOIN_TOL.max(1e-9);
    let mut nodes: Vec<Point> = Vec::new();
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let mut members: Vec<Vec<usize>> = Vec::new();
    let mut ends: Vec<(usize, usize)> = Vec::new();
    for (i, p) in open.iter().enumerate() {
        let a = node_id(&mut nodes, &mut grid, p.points[0], tol);
        let b = node_id(&mut nodes, &mut grid, *p.points.last().expect("open paths have two ends"), tol);
        while members.len() < nodes.len() {
            members.push(Vec::new());
        }
        members[a].push(i);
        members[b].push(i);
        ends.push((a, b));
    }
    let mut parent: Vec<usize> = (0..open.len()).collect();
    for list in &members {
        for pair in list.windows(2) {
            union(&mut parent, pair[0], pair[1]);
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..open.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let mut comps: Vec<Vec<usize>> = groups.into_values().collect();
    comps.sort_by_key(|c| c.iter().map(|&i| (open[i].shape_order, open[i].subpath)).min().unwrap_or((0, 0)));
    comps.into_iter().map(|comp| chain_issue(doc, open, &nodes, &ends, &comp)).collect()
}

/// The graph node for a point, or a new one. Nodes are points within `tol` of each other, found
/// through a grid of cells `tol` across.
fn node_id(nodes: &mut Vec<Point>, grid: &mut HashMap<(i64, i64), Vec<usize>>, p: Point, tol: f64) -> usize {
    let cell = |p: Point| ((p.x / tol).floor() as i64, (p.y / tol).floor() as i64);
    let (cx, cy) = cell(p);
    for dx in -1..=1 {
        for dy in -1..=1 {
            if let Some(list) = grid.get(&(cx + dx, cy + dy))
                && let Some(&i) = list.iter().find(|&&i| nodes[i].distance(p) <= tol)
            {
                return i;
            }
        }
    }
    nodes.push(p);
    grid.entry((cx, cy)).or_default().push(nodes.len() - 1);
    nodes.len() - 1
}

fn chain_issue(doc: &Document, open: &[OpenPath], nodes: &[Point], ends: &[(usize, usize)], comp: &[usize]) -> ((usize, usize), Issue) {
    // How many of the group's ends meet at each node; one is a loose end, more than two a branch.
    let mut used: Vec<usize> = Vec::new();
    let mut seen = vec![false; nodes.len()];
    let mut degree = vec![0usize; nodes.len()];
    for &i in comp {
        for n in [ends[i].0, ends[i].1] {
            if !seen[n] {
                seen[n] = true;
                used.push(n);
            }
            degree[n] += 1;
        }
    }
    let loose: Vec<usize> = used.iter().copied().filter(|&n| degree[n] == 1).collect();
    let branch: Vec<usize> = used.iter().copied().filter(|&n| degree[n] > 2).collect();

    let highlights: Vec<Polyline> = comp.iter().map(|&i| Polyline { points: open[i].points.clone(), closed: false }).collect();
    let shapes: BTreeSet<u64> = comp.iter().map(|&i| open[i].shape).collect();
    let layer = open[comp[0]].layer;
    let all_bb = bounds_of(comp.iter().flat_map(|&i| open[i].points.iter().copied())).unwrap_or(Rect::ZERO);
    // Enough of the group to identify it while the problem stays put, without a huge key for a
    // drawing made of thousands of tiny lines.
    let mut comp_key = comp.iter().take(32).map(|&i| format!("{}.{}", open[i].shape, open[i].subpath)).collect::<Vec<_>>().join(",");
    if comp.len() > 32 {
        comp_key.push_str(&format!("+{}", comp.len() - 32));
    }
    let order = comp.iter().map(|&i| (open[i].shape_order, open[i].subpath)).min().unwrap_or((0, 0));
    let fills = comp.iter().all(|&i| doc.layers.get(open[i].layer).is_some_and(|l| l.mode == LayerMode::Fill));

    if loose.is_empty() {
        if let Some(&n) = branch.first() {
            let p = nodes[n];
            return (
                order,
                Issue {
                    key: format!("branch:{comp_key}"),
                    kind: IssueKind::OpenEnds,
                    hint: "The lines meet at a point where more than two ends come together".into(),
                    message: with_fill(
                        fills,
                        "These lines meet at a point where more than two ends come together, so they do not form one simple closed outline. Remove the extra line, or join only the lines that make the outline.".into(),
                    ),
                    shapes,
                    layer,
                    focus: all_bb.inflate(2.0, 2.0),
                    marker: Some(p),
                    gap: None,
                    highlights,
                    fix: None,
                    fills,
                },
            );
        }
        if comp.len() == 1 {
            let only = &open[comp[0]];
            if only.points.len() < 3 || double_area(&only.points).abs() < 2e-6 {
                let bb = bounds_of(only.points.iter().copied()).unwrap_or(all_bb);
                return (
                    order,
                    Issue {
                        key: format!("loop:{comp_key}"),
                        kind: IssueKind::Degenerate,
                        hint: "The path loops back on itself with no area".into(),
                        message: with_fill(fills, "This path loops back on itself with no area, so there is nothing to cut. Remove it, or draw a proper closed outline.".into()),
                        shapes,
                        layer,
                        focus: bb.inflate(2.0, 2.0),
                        marker: Some(bb.center()),
                        gap: None,
                        highlights,
                        fix: None,
                        fills,
                    },
                );
            }
            let junction = only.points[0];
            return (
                order,
                Issue {
                    key: format!("not-closed:{comp_key}"),
                    kind: IssueKind::NotClosed,
                    hint: "The ends meet, but the outline is not closed".into(),
                    message: with_fill(
                        fills,
                        "The two ends of this outline already meet here, but the outline is marked as open. Select it and press Close Paths (Tools > Close Paths), or use the Fix button.".into(),
                    ),
                    shapes,
                    layer,
                    focus: Rect::new(junction.x - 5.0, junction.y - 5.0, junction.x + 5.0, junction.y + 5.0),
                    marker: Some(junction),
                    gap: Some((junction, junction)),
                    highlights,
                    fix: Some(Fix::Close),
                    fills,
                },
            );
        }
        let junction = open[comp[0]].points[0];
        let lines = shapes.len();
        return (
            order,
            Issue {
                key: format!("not-joined:{comp_key}"),
                kind: IssueKind::NotClosed,
                hint: format!("{lines} open lines meet end to end, but they are not joined"),
                message: with_fill(
                    fills,
                    format!(
                        "These {lines} open lines meet end to end at every corner, but they are still separate shapes. Join them into one closed outline (Tools > Auto-Join), or use the Fix button."
                    ),
                ),
                shapes,
                layer,
                focus: all_bb.inflate(2.0, 2.0),
                marker: Some(junction),
                gap: None,
                highlights,
                fix: if lines > 1 { Some(Fix::Join { lines }) } else { None },
                fills,
            },
        );
    }

    if loose.len() == 2 {
        let (a, b) = (nodes[loose[0]], nodes[loose[1]]);
        let gap = a.distance(b);
        let only = (comp.len() == 1).then(|| &open[comp[0]]);
        let single_line = only.is_some_and(|o| o.points.len() == 2);
        let hint = if single_line {
            "An open line is not a closed outline".to_string()
        } else if gap <= 1.0 {
            format!("The outline is open by {gap:.2} mm here")
        } else {
            format!("The outline is open: its ends are {gap:.1} mm apart")
        };
        let message = if single_line {
            "This is an open line, not a closed outline. Draw it as a closed shape, or select it with the lines that complete the outline and press Auto-Join (Tools > Auto-Join).".to_string()
        } else if comp.len() == 1 {
            format!(
                "The outline is open by {gap:.2} mm between its two ends. Close it with a straight line (the Fix button), or drag the two ends together."
            )
        } else {
            format!(
                "These {} open lines almost make an outline, but there is a {gap:.2} mm gap here. Drag the two ends together, then join them (Tools > Auto-Join).",
                shapes.len()
            )
        };
        let pad = (gap * 0.35).max(1.5);
        return (
            order,
            Issue {
                key: format!("open:{comp_key}"),
                kind: IssueKind::OpenEnds,
                hint,
                message: with_fill(fills, message),
                shapes,
                layer,
                focus: Rect::new(a.x.min(b.x) - pad, a.y.min(b.y) - pad, a.x.max(b.x) + pad, a.y.max(b.y) + pad),
                marker: Some(mid(a, b)),
                gap: Some((a, b)),
                highlights,
                fix: if only.is_some() && !single_line { Some(Fix::CloseGap) } else { None },
                fills,
            },
        );
    }

    // Several loose ends: usually a selection of unrelated lines rather than one outline.
    let loose_bb = bounds_of(loose.iter().map(|&n| nodes[n])).unwrap_or(all_bb);
    let count = loose.len();
    (
        order,
        Issue {
            key: format!("loose:{comp_key}"),
            kind: IssueKind::OpenEnds,
            hint: format!("These lines do not form one closed outline ({count} loose ends)"),
            message: with_fill(
                fills,
                format!("The selected lines do not form one closed outline: they have {count} loose ends. Join the lines that touch, and remove or redraw the extras."),
            ),
            shapes,
            layer,
            focus: loose_bb.inflate(2.0, 2.0),
            marker: Some(loose_bb.center()),
            gap: None,
            highlights,
            fix: None,
            fills,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ShapeKind, edit};
    use bc_geom::{Affine, polyline_path};

    fn add(doc: &mut Document, points: &[Point], closed: bool) -> u64 {
        doc.add_shape(ShapeKind::Path(polyline_path(points, closed)), Affine::IDENTITY, 0)
    }

    fn line(doc: &mut Document, a: (f64, f64), b: (f64, f64)) -> u64 {
        add(doc, &[Point::new(a.0, a.1), Point::new(b.0, b.1)], false)
    }

    /// Four separate lines around a square, the way a DXF export leaves an outline.
    fn square(doc: &mut Document, size: f64) {
        line(doc, (0.0, 0.0), (size, 0.0));
        line(doc, (size, 0.0), (size, size));
        line(doc, (size, size), (0.0, size));
        line(doc, (0.0, size), (0.0, 0.0));
    }

    #[test]
    fn four_separate_lines_are_one_unjoined_outline() {
        let mut doc = Document::default();
        line(&mut doc, (0.0, 0.0), (20.0, 0.0));
        line(&mut doc, (20.0, 0.0), (20.0, 10.0));
        line(&mut doc, (20.0, 10.0), (0.0, 10.0));
        line(&mut doc, (0.0, 10.0), (0.0, 0.0));
        let found = issues(&doc);
        assert_eq!(found.len(), 1, "one outline, one problem: {found:?}");
        assert_eq!(found[0].kind, IssueKind::NotClosed);
        assert_eq!(found[0].shapes.len(), 4);
        assert_eq!(found[0].highlights.len(), 4);
        assert_eq!(found[0].fix, Some(Fix::Join { lines: 4 }));
        assert_eq!(found[0].fix_label().as_deref(), Some("Join 4 lines"));
        assert!(found[0].message.contains("Auto-Join"), "{}", found[0].message);
        assert!(found[0].focus.x0 <= 0.0 && found[0].focus.x1 >= 20.0);
    }

    #[test]
    fn ends_that_meet_but_are_not_closed_are_found_at_the_junction() {
        let mut doc = Document::default();
        // The ends meet to within 0.02 mm, but the outline was never flagged closed.
        add(&mut doc, &[Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0), Point::new(0.0, 0.02)], false);
        let found = issues(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, IssueKind::NotClosed);
        assert!(found[0].hint.contains("not closed"), "{}", found[0].hint);
        assert_eq!(found[0].fix, Some(Fix::Close));
        let (a, b) = found[0].gap.expect("the junction is marked");
        assert!(a.distance(b) <= JOIN_TOL);
    }

    #[test]
    fn a_real_gap_is_marked_with_both_ends_and_can_be_closed() {
        let mut doc = Document::default();
        // A rectangle whose last corner stops 2 mm short of the first one.
        let id = add(&mut doc, &[Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0), Point::new(0.0, 10.0), Point::new(0.0, 2.0)], false);
        let found = issues(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, IssueKind::OpenEnds);
        let (a, b) = found[0].gap.expect("the gap ends are marked");
        assert!((a.distance(b) - 2.0).abs() < 1e-6, "{}", a.distance(b));
        assert!(found[0].message.contains("2.00 mm"), "{}", found[0].message);
        assert_eq!(found[0].marker, Some(mid(a, b)));
        assert_eq!(found[0].fix, Some(Fix::CloseGap));

        // The repair closes the outline with a straight line across the gap.
        let shapes = found[0].shapes.clone();
        assert_eq!(edit::close_paths(&mut doc, &shapes, f64::INFINITY), 1);
        assert!(doc.shape(id).expect("still there").is_closed());
        assert!(issues(&doc).is_empty(), "the repair clears the problem");
    }

    #[test]
    fn a_valid_closed_outline_has_no_issues() {
        let mut doc = Document::default();
        add(&mut doc, &[Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0), Point::new(0.0, 10.0)], true);
        assert!(issues(&doc).is_empty());
    }

    #[test]
    fn a_degenerate_closed_path_is_reported() {
        let mut doc = Document::default();
        add(&mut doc, &[Point::new(0.0, 0.0), Point::new(5.0, 0.0)], true);
        let found = issues(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, IssueKind::Degenerate);
        assert!(found[0].hint.contains("3 points"), "{}", found[0].hint);
        assert_eq!(found[0].fix, None);
    }

    #[test]
    fn a_lone_straight_segment_is_a_cut_line_not_a_broken_outline() {
        let mut doc = Document::default();
        line(&mut doc, (0.0, 0.0), (10.0, 0.0));
        assert!(issues(&doc).is_empty(), "one straight cut line is not an outline that failed to close");
        // On a Fill layer there is nothing to fill, so it is reported after all.
        doc.layers[0].mode = LayerMode::Fill;
        let found = issues(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, IssueKind::OpenEnds);
        assert!(found[0].hint.contains("open line"), "{}", found[0].hint);
        assert_eq!(found[0].fix, None, "closing a line back on itself would make no area");
    }

    #[test]
    fn a_segment_that_helps_make_an_outline_is_reported_with_the_rest() {
        // Three sides of a square: two segments and a gap across the fourth.
        let mut doc = Document::default();
        line(&mut doc, (0.0, 0.0), (10.0, 0.0));
        line(&mut doc, (10.0, 0.0), (10.0, 10.0));
        line(&mut doc, (10.0, 10.0), (0.0, 10.0));
        let found = issues(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, IssueKind::OpenEnds);
        assert_eq!(found[0].shapes.len(), 3);
        assert!(found[0].message.contains("gap"), "{}", found[0].message);
        assert_eq!(found[0].fix, None, "a gap between separate lines needs the ends dragged together");
    }

    #[test]
    fn shapes_that_are_not_sent_to_the_laser_are_left_alone() {
        let mut doc = Document::default();
        line(&mut doc, (0.0, 0.0), (20.0, 0.0));
        line(&mut doc, (20.0, 0.0), (20.0, 10.0));
        assert_eq!(issues(&doc).len(), 1);
        doc.layers[0].output = false;
        assert!(issues(&doc).is_empty());
        doc.layers[0].output = true;
        // A dimension or jig slot on a tool layer is never cut, so it is never broken.
        for s in &mut doc.shapes {
            s.layer = crate::CUT_LAYER_COUNT;
        }
        assert!(issues(&doc).is_empty());
    }

    #[test]
    fn a_fill_layer_says_why_the_open_outline_matters() {
        let mut doc = Document::default();
        line(&mut doc, (0.0, 0.0), (20.0, 0.0));
        line(&mut doc, (20.0, 0.0), (20.0, 10.0));
        doc.layers[0].mode = LayerMode::Fill;
        let found = issues(&doc);
        assert_eq!(found.len(), 1);
        assert!(found[0].fills);
        assert!(found[0].message.starts_with("This outline is on a Fill layer"), "{}", found[0].message);
    }

    #[test]
    fn issue_keys_stay_put_when_the_problem_moves() {
        let mut doc = Document::default();
        add(&mut doc, &[Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0), Point::new(0.0, 12.0)], false);
        let before = issues(&doc)[0].key.clone();
        let all: BTreeSet<u64> = doc.shapes.iter().map(|s| s.id).collect();
        edit::translate(&mut doc, &all, bc_geom::Vec2::new(25.0, -4.0));
        let after = issues(&doc)[0].key.clone();
        assert_eq!(before, after, "the same violation keeps its identity so stepping can follow it");
    }

    #[test]
    fn eighteen_lines_join_into_one_closed_outline() {
        let n = 18;
        let mut doc = Document::default();
        let pts: Vec<Point> = (0..n)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / n as f64;
                Point::new(50.0 + 30.0 * a.cos(), 50.0 + 20.0 * a.sin())
            })
            .collect();
        for i in 0..n {
            add(&mut doc, &[pts[i], pts[(i + 1) % n]], false);
        }
        let found = issues(&doc);
        assert_eq!(found.len(), 1, "one outline");
        assert_eq!(found[0].fix, Some(Fix::Join { lines: n }));
        let joined = edit::auto_join(&mut doc, &found[0].shapes, JOIN_TOL);
        assert_eq!(joined.len(), 1, "all {n} lines become one shape");
        let shape = doc.shape(*joined.iter().next().unwrap()).expect("the joined shape");
        assert!(shape.is_closed(), "the joined shape is closed");
        assert!(issues(&doc).is_empty(), "nothing left to report");
    }

    #[test]
    fn an_issue_belongs_to_the_layer_its_outline_starts_on() {
        // The Layers panel marks the layer that has the problem.
        let mut doc = Document::default();
        line(&mut doc, (0.0, 0.0), (10.0, 0.0));
        line(&mut doc, (10.0, 0.0), (10.0, 10.0));
        for s in &mut doc.shapes {
            s.layer = 3;
        }
        let found = issues(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].layer, 3);
    }

    #[test]
    fn a_double_click_takes_the_whole_touching_run() {
        let mut doc = Document::default();
        square(&mut doc, 10.0);
        let far = line(&mut doc, (30.0, 30.0), (40.0, 30.0));
        let run = connected(&doc, doc.shapes[0].id, JOIN_TOL);
        assert_eq!(run.len(), 4, "the four sides of the square");
        assert!(!run.contains(&far), "a separate line stays out");
    }

    #[test]
    fn a_gap_breaks_the_run_but_a_crossing_does_not() {
        let mut doc = Document::default();
        let a = line(&mut doc, (0.0, 0.0), (10.0, 0.0));
        line(&mut doc, (10.0, 0.0), (10.0, 10.0));
        line(&mut doc, (10.0, 10.0), (0.0, 10.0));
        // 0.5 mm away: too far to count as joined.
        let detached = line(&mut doc, (10.0, 10.5), (0.0, 10.5));
        assert_eq!(connected(&doc, a, JOIN_TOL).len(), 3);
        assert!(!connected(&doc, a, JOIN_TOL).contains(&detached));

        let mut doc = Document::default();
        let a = line(&mut doc, (0.0, 0.0), (10.0, 10.0));
        line(&mut doc, (0.0, 10.0), (10.0, 0.0));
        assert_eq!(connected(&doc, a, JOIN_TOL).len(), 2, "crossing lines touch");
    }

    #[test]
    fn curves_are_compared_all_the_way_round() {
        // Two circles 0.5 mm apart are close, but not touching.
        let mut doc = Document::default();
        let a = doc.add_shape(ShapeKind::Path(bc_geom::ellipse_path(5.0, 5.0)), Affine::IDENTITY, 0);
        doc.add_shape(ShapeKind::Path(bc_geom::ellipse_path(5.0, 5.0)), Affine::translate(bc_geom::Vec2::new(10.5, 0.0)), 0);
        assert_eq!(connected(&doc, a, JOIN_TOL).len(), 1);
        // The same two circles, touching.
        let mut doc = Document::default();
        let a = doc.add_shape(ShapeKind::Path(bc_geom::ellipse_path(5.0, 5.0)), Affine::IDENTITY, 0);
        doc.add_shape(ShapeKind::Path(bc_geom::ellipse_path(5.0, 5.0)), Affine::translate(bc_geom::Vec2::new(10.0, 0.0)), 0);
        assert_eq!(connected(&doc, a, JOIN_TOL).len(), 2);
    }

    #[test]
    fn a_t_junction_counts_as_touching() {
        let mut doc = Document::default();
        let a = line(&mut doc, (0.0, 0.0), (10.0, 0.0));
        line(&mut doc, (5.0, 0.0), (5.0, 8.0));
        assert_eq!(connected(&doc, a, JOIN_TOL).len(), 2);
    }

    #[test]
    fn tool_layer_shapes_stay_out_of_a_design_run() {
        let mut doc = Document::default();
        let a = line(&mut doc, (0.0, 0.0), (10.0, 0.0));
        let slot = line(&mut doc, (10.0, 0.0), (10.0, 10.0));
        doc.shapes.iter_mut().find(|s| s.id == slot).expect("the slot").layer = crate::CUT_LAYER_COUNT;
        assert_eq!(connected(&doc, a, JOIN_TOL).len(), 1);
    }

    #[test]
    fn a_locked_shape_is_not_dragged_into_the_run() {
        let mut doc = Document::default();
        let a = line(&mut doc, (0.0, 0.0), (10.0, 0.0));
        let b = line(&mut doc, (10.0, 0.0), (10.0, 10.0));
        doc.shapes.iter_mut().find(|s| s.id == b).expect("the second line").locked = true;
        assert_eq!(connected(&doc, a, JOIN_TOL).len(), 1);
    }
}

