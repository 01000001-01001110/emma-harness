//! A small SVG writer, and a layered layout that places boxes from a graph.
//!
//! Positions are computed rather than typed. The hand-drawn diagrams carried
//! coordinates like `x="310" y="14"`, so moving a crate meant re-typing
//! numbers by hand, and the picture drifted while still looking authoritative.
//!
//! This is not a graph-layout library. Every diagram here is a handful of
//! nodes in a few layers, which layered placement with centred rows covers.
//! A graph needing a force-directed layout to be legible is one a reader will
//! not follow either.

use std::fmt::Write as _;

use crate::theme;

/// How much a relationship is worth drawing attention to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// The ordinary case.
    Plain,
    /// The one thing this diagram is about. One per diagram -- see
    /// [`theme::Palette::ACCENT`].
    Accent,
    /// Real but conditional: a dev-dependency, a platform-only path. Dashed
    /// as well as recoloured, since colour alone is not a distinction every
    /// reader can see.
    Dim,
}

pub struct Node {
    pub id: String,
    pub label: String,
    pub weight: Weight,
}

pub struct Edge {
    pub from: String,
    pub to: String,
    pub weight: Weight,
    /// Drawn beside the edge's midpoint. Empty for most.
    pub label: String,
}

/// A graph plus the sentence a screen reader gets instead of the picture.
///
/// `caption` is required and written by hand. An `aria-label` assembled from
/// node names reads as a list of nouns and says nothing about the shape, which
/// is what a sighted reader takes from the picture.
pub struct Diagram {
    pub prefix: String,
    pub caption: String,
    pub layers: Vec<Vec<Node>>,
    pub edges: Vec<Edge>,
}

const BOX_W: f64 = 150.0;
const BOX_H: f64 = 38.0;
const GAP_X: f64 = 22.0;
const GAP_Y: f64 = 64.0;
const PAD: f64 = 16.0;

struct Placed {
    x: f64,
    y: f64,
    label: String,
    weight: Weight,
}

impl Diagram {
    /// Emit the whole `<svg>`, self-contained.
    pub fn render(&self) -> String {
        let p = &self.prefix;
        let widest = self
            .layers
            .iter()
            .map(|l| l.len())
            .max()
            .unwrap_or(1)
            .max(1) as f64;
        let width = PAD * 2.0 + widest * BOX_W + (widest - 1.0) * GAP_X;
        let height = PAD * 2.0
            + self.layers.len() as f64 * BOX_H
            + (self.layers.len().saturating_sub(1)) as f64 * GAP_Y;

        let mut placed: Vec<(String, Placed)> = Vec::new();
        for (row, layer) in self.layers.iter().enumerate() {
            // Rows are centred against the widest row, so a three-box row above
            // a five-box row reads as a hierarchy rather than as left-aligned
            // debris.
            let n = layer.len() as f64;
            let row_w = n * BOX_W + (n - 1.0) * GAP_X;
            let start = (width - row_w) / 2.0;
            for (i, node) in layer.iter().enumerate() {
                placed.push((
                    node.id.clone(),
                    Placed {
                        x: start + i as f64 * (BOX_W + GAP_X),
                        y: PAD + row as f64 * (BOX_H + GAP_Y),
                        label: node.label.clone(),
                        weight: node.weight,
                    },
                ));
            }
        }

        let mut s = String::new();
        let _ = writeln!(
            s,
            "<svg viewBox=\"0 0 {width:.0} {height:.0}\" role=\"img\" aria-label=\"{}\">",
            escape(&self.caption)
        );
        s.push_str(&theme::style(p));
        let _ = writeln!(
            s,
            "  <defs>\n\
             \x20   <marker id=\"{p}-tip\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"6\" markerHeight=\"6\" orient=\"auto-start-reverse\">\n\
             \x20     <path d=\"M 0 0 L 10 5 L 0 10 z\" fill=\"var(--{p}-line)\"/>\n\
             \x20   </marker>\n\
             \x20   <marker id=\"{p}-tip-hl\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"6\" markerHeight=\"6\" orient=\"auto-start-reverse\">\n\
             \x20     <path d=\"M 0 0 L 10 5 L 0 10 z\" fill=\"var(--{p}-accent)\"/>\n\
             \x20   </marker>\n\
             \x20   <marker id=\"{p}-tip-dim\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"6\" markerHeight=\"6\" orient=\"auto-start-reverse\">\n\
             \x20     <path d=\"M 0 0 L 10 5 L 0 10 z\" fill=\"var(--{p}-dim)\"/>\n\
             \x20   </marker>\n\
             \x20 </defs>\n\
             \x20 <rect class=\"{p}-ground\" x=\"0\" y=\"0\" width=\"{width:.0}\" height=\"{height:.0}\"/>\n"
        );

        // Edges first: a box drawn over a line is a box with a clean border,
        // and a line drawn over a box crosses its label.
        for e in &self.edges {
            let (Some(a), Some(b)) = (find(&placed, &e.from), find(&placed, &e.to)) else {
                continue;
            };
            let (cls, tip) = match e.weight {
                Weight::Plain => (format!("{p}-edge"), format!("{p}-tip")),
                Weight::Accent => (format!("{p}-edge-hl"), format!("{p}-tip-hl")),
                Weight::Dim => (format!("{p}-edge-dim"), format!("{p}-tip-dim")),
            };
            let (x1, y1, x2, y2) = anchor(a, b);
            let _ = writeln!(
                s,
                "  <path class=\"{cls}\" marker-end=\"url(#{tip})\" d=\"M {x1:.0} {y1:.0} L {x2:.0} {y2:.0}\"/>"
            );
            if !e.label.is_empty() {
                let _ = writeln!(
                    s,
                    "  <text class=\"{p}-note\" x=\"{:.0}\" y=\"{:.0}\" text-anchor=\"middle\">{}</text>",
                    (x1 + x2) / 2.0,
                    (y1 + y2) / 2.0 - 4.0,
                    escape(&e.label)
                );
            }
        }

        for (_, n) in &placed {
            let cls = match n.weight {
                Weight::Accent => format!("{p}-box-hl"),
                _ => format!("{p}-box"),
            };
            let _ = writeln!(
                s,
                "  <rect class=\"{cls}\" x=\"{:.0}\" y=\"{:.0}\" width=\"{BOX_W:.0}\" height=\"{BOX_H:.0}\" rx=\"3\"/>\n  <text class=\"{p}-lbl\" x=\"{:.0}\" y=\"{:.0}\" text-anchor=\"middle\">{}</text>\n",
                n.x,
                n.y,
                n.x + BOX_W / 2.0,
                n.y + BOX_H / 2.0 + 4.5,
                escape(&n.label)
            );
        }

        s.push_str("</svg>");
        s
    }
}

fn find<'a>(placed: &'a [(String, Placed)], id: &str) -> Option<&'a Placed> {
    placed.iter().find(|(k, _)| k == id).map(|(_, v)| v)
}

/// Where an edge leaves one box and meets another.
///
/// Anchored to the facing edges rather than to centres, so the arrowhead lands
/// on the border instead of under the label.
fn anchor(a: &Placed, b: &Placed) -> (f64, f64, f64, f64) {
    let (ax, ay) = (a.x + BOX_W / 2.0, a.y + BOX_H / 2.0);
    let (bx, by) = (b.x + BOX_W / 2.0, b.y + BOX_H / 2.0);
    if (a.y - b.y).abs() < 1.0 {
        // Same row: leave and arrive on the sides.
        let (x1, x2) = if ax < bx {
            (a.x + BOX_W, b.x)
        } else {
            (a.x, b.x + BOX_W)
        };
        (x1, ay, x2, by)
    } else if a.y < b.y {
        (ax, a.y + BOX_H, bx, b.y)
    } else {
        (ax, a.y, bx, b.y + BOX_H)
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            label: id.into(),
            weight: Weight::Plain,
        }
    }

    fn two_layers() -> Diagram {
        Diagram {
            prefix: "t".into(),
            caption: "a over b".into(),
            layers: vec![vec![node("a")], vec![node("b"), node("c")]],
            edges: vec![Edge {
                from: "a".into(),
                to: "b".into(),
                weight: Weight::Plain,
                label: String::new(),
            }],
        }
    }

    /// **If this breaks:** a diagram is emitted with no alternative text, and
    /// the only readers who notice are the ones who cannot see it.
    #[test]
    fn the_caption_becomes_the_aria_label() {
        let s = two_layers().render();
        assert!(s.contains("aria-label=\"a over b\""), "{s}");
    }

    /// **If this breaks:** a label containing `<` or `&` -- a crate named
    /// `tools/fs & co`, a caption with a comparison in it -- produces an SVG
    /// that will not parse, and the page shows nothing at all.
    #[test]
    fn markup_in_a_label_cannot_escape_into_the_document() {
        let d = Diagram {
            prefix: "t".into(),
            caption: "a & b".into(),
            layers: vec![vec![Node {
                id: "x".into(),
                label: "<script>".into(),
                weight: Weight::Plain,
            }]],
            edges: vec![],
        };
        let s = d.render();
        assert!(s.contains("&lt;script&gt;"), "{s}");
        assert!(!s.contains("<script>"), "{s}");
        assert!(s.contains("a &amp; b"), "{s}");
    }

    /// **If this breaks:** rows are left-aligned and a hierarchy reads as a
    /// ragged list.
    #[test]
    fn a_narrow_row_is_centred_over_a_wide_one() {
        let d = two_layers();
        let s = d.render();
        // The single box in the top row starts further right than the first
        // box of the two-box row below it.
        let xs: Vec<f64> = s
            .lines()
            .filter(|l| l.contains("<rect class=\"t-box\""))
            .filter_map(|l| {
                l.split("x=\"")
                    .nth(1)
                    .and_then(|r| r.split('"').next())
                    .and_then(|v| v.parse().ok())
            })
            .collect();
        assert_eq!(xs.len(), 3, "three boxes: {s}");
        assert!(xs[0] > xs[1], "the lone box should be centred: {xs:?}");
    }

    /// **If this breaks:** an edge naming a node that does not exist silently
    /// draws from the origin, producing a line across the whole picture that
    /// looks deliberate.
    #[test]
    fn an_edge_to_an_unknown_node_draws_nothing() {
        let mut d = two_layers();
        d.edges.push(Edge {
            from: "a".into(),
            to: "nowhere".into(),
            weight: Weight::Plain,
            label: String::new(),
        });
        let s = d.render();
        assert_eq!(s.matches("<path class=\"t-edge\"").count(), 1, "{s}");
    }

    /// **If this breaks:** a conditional relationship is distinguished by
    /// colour alone, which a reader with a colour deficiency cannot see.
    #[test]
    fn a_dim_edge_is_dashed_and_not_only_recoloured() {
        let mut d = two_layers();
        d.edges[0].weight = Weight::Dim;
        let s = d.render();
        assert!(s.contains("stroke-dasharray"), "{s}");
    }
}
