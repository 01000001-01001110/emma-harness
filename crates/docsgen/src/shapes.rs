//! The three shapes the remaining diagrams need, on top of the layered graph.
//!
//! Each takes items read from source by [`crate::rust`] and returns a
//! [`Diagram`], so the layout stays in one place and the per-diagram code is
//! about *which* facts rather than about geometry.
//!
//! # Why three and not one general one
//!
//! A ladder, a set and a chain answer different questions, and drawing all
//! three as a layered graph makes each of them worse. An ordered sequence of
//! checks drawn as free-floating boxes loses the order, which is the only fact
//! it carries.
//!
//! When a diagram fits none of these, that is a signal about the diagram. A
//! picture needing a bespoke layout is usually a picture arguing something the
//! source does not state.

use crate::rust::Item;
use crate::svg::{Diagram, Edge, Node, Weight};

/// An ordered sequence, drawn top to bottom, each step pointing at the next.
///
/// For a function that checks things in a fixed order: the approval ladder,
/// the gates before a write. The order is the fact, so it is the layout.
///
/// `accent` names the step the page is about, by item name. A step name that
/// matches nothing leaves every box plain rather than failing, because the
/// emphasis is a presentation choice and losing it should not lose the
/// diagram.
pub fn ladder(prefix: &str, caption: String, steps: &[Item], accent: Option<&str>) -> Diagram {
    let layers = steps
        .iter()
        .map(|s| {
            vec![Node {
                id: s.name.clone(),
                label: label_for(s),
                weight: if accent == Some(s.name.as_str()) {
                    Weight::Accent
                } else {
                    Weight::Plain
                },
            }]
        })
        .collect();
    let edges = steps
        .windows(2)
        .map(|w| Edge {
            from: w[0].name.clone(),
            to: w[1].name.clone(),
            weight: Weight::Plain,
            label: String::new(),
        })
        .collect();
    Diagram {
        prefix: prefix.to_string(),
        caption,
        layers,
        edges,
    }
}

/// An unordered set, drawn in rows, with no edges.
///
/// For the variants of an enum or the fields of a struct where the membership
/// is the fact and nothing connects them. Drawing edges between them would
/// invent a relationship the source does not state.
///
/// `per_row` wraps: five states in one row of five is unreadable at the width
/// a docs page gives.
pub fn set(
    prefix: &str,
    caption: String,
    items: &[Item],
    per_row: usize,
    accent: Option<&str>,
) -> Diagram {
    let per_row = per_row.max(1);
    let layers = items
        .chunks(per_row)
        .map(|chunk| {
            chunk
                .iter()
                .map(|i| Node {
                    id: i.name.clone(),
                    label: label_for(i),
                    weight: if accent == Some(i.name.as_str()) {
                        Weight::Accent
                    } else {
                        Weight::Plain
                    },
                })
                .collect()
        })
        .collect();
    Diagram {
        prefix: prefix.to_string(),
        caption,
        layers,
        edges: Vec::new(),
    }
}

/// A source fanning out to several destinations, or several converging on one.
///
/// `from` above, `to` below, one edge each. `dim` names destinations whose
/// edge is conditional -- drawn dashed, since colour alone is not a
/// distinction every reader can see.
pub fn fan(prefix: &str, caption: String, from: &Item, to: &[Item], dim: &[&str]) -> Diagram {
    let head = Node {
        id: from.name.clone(),
        label: label_for(from),
        weight: Weight::Accent,
    };
    let tail: Vec<Node> = to
        .iter()
        .map(|i| Node {
            id: i.name.clone(),
            label: label_for(i),
            weight: Weight::Plain,
        })
        .collect();
    let edges = to
        .iter()
        .map(|i| Edge {
            from: from.name.clone(),
            to: i.name.clone(),
            weight: if dim.contains(&i.name.as_str()) {
                Weight::Dim
            } else {
                Weight::Plain
            },
            label: String::new(),
        })
        .collect();
    Diagram {
        prefix: prefix.to_string(),
        caption,
        layers: vec![vec![head], tail],
        edges,
    }
}

/// What goes in the box.
///
/// The name, and the detail when there is one and it is short enough to sit
/// beside it. A doc sentence is not used as a label: it belongs in the caption
/// or the prose, and a box wide enough to hold a sentence stops being a box.
fn label_for(i: &Item) -> String {
    const ROOM: usize = 18;
    if i.detail.is_empty() || i.name.len() + i.detail.len() > ROOM {
        i.name.clone()
    } else {
        format!("{}: {}", i.name, i.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> Vec<Item> {
        names
            .iter()
            .map(|n| Item {
                name: (*n).into(),
                doc: String::new(),
                detail: String::new(),
            })
            .collect()
    }

    /// **If this breaks:** an ordered sequence of checks is drawn without its
    /// order, which is the only thing it had to say.
    #[test]
    fn a_ladder_is_one_step_per_row_and_each_points_at_the_next() {
        let d = ladder("t", "c".into(), &items(&["a", "b", "c"]), None);
        assert_eq!(d.layers.len(), 3);
        assert!(d.layers.iter().all(|l| l.len() == 1), "one per row");
        assert_eq!(d.edges.len(), 2, "n-1 edges");
        assert_eq!(d.edges[0].from, "a");
        assert_eq!(d.edges[0].to, "b");
    }

    /// **If this breaks:** a set of unrelated variants is drawn with edges
    /// between them, inventing a relationship the source does not state.
    #[test]
    fn a_set_has_no_edges() {
        let d = set("t", "c".into(), &items(&["a", "b", "c", "d"]), 2, None);
        assert!(d.edges.is_empty(), "{:?}", d.edges.len());
        assert_eq!(d.layers.len(), 2, "four items, two per row");
    }

    /// **If this breaks:** a single-step ladder or an empty set panics on a
    /// `windows(2)` or a zero chunk size, and the generator dies on a diagram
    /// rather than drawing it.
    #[test]
    fn one_item_and_a_zero_row_width_are_both_survivable() {
        let d = ladder("t", "c".into(), &items(&["only"]), None);
        assert!(d.edges.is_empty());
        let d = set("t", "c".into(), &items(&["a"]), 0, None);
        assert_eq!(d.layers.len(), 1);
    }

    /// **If this breaks:** the emphasis lands on nothing and the reader is not
    /// told which box the page is about.
    #[test]
    fn the_accent_marks_the_named_step_and_only_that_one() {
        let d = ladder("t", "c".into(), &items(&["a", "b"]), Some("b"));
        assert!(matches!(d.layers[0][0].weight, Weight::Plain));
        assert!(matches!(d.layers[1][0].weight, Weight::Accent));
        // A name that matches nothing must not fail the diagram.
        let d = ladder("t", "c".into(), &items(&["a"]), Some("absent"));
        assert!(matches!(d.layers[0][0].weight, Weight::Plain));
    }

    /// **If this breaks:** a long type overflows its box and overlaps the one
    /// beside it.
    #[test]
    fn a_label_drops_a_detail_that_will_not_fit() {
        let short = Item {
            name: "a".into(),
            doc: String::new(),
            detail: "u32".into(),
        };
        let long = Item {
            name: "connection".into(),
            doc: String::new(),
            detail: "Option<HashMap<String, Vec<u8>>>".into(),
        };
        let d = set("t", "c".into(), &[short, long], 2, None);
        assert_eq!(d.layers[0][0].label, "a: u32");
        assert_eq!(d.layers[0][1].label, "connection");
    }

    /// **If this breaks:** a conditional edge is distinguished by colour
    /// alone.
    #[test]
    fn a_fan_marks_its_conditional_edges_dim() {
        let from = items(&["src"]).remove(0);
        let d = fan("t", "c".into(), &from, &items(&["a", "b"]), &["b"]);
        assert!(matches!(d.edges[0].weight, Weight::Plain));
        assert!(matches!(d.edges[1].weight, Weight::Dim));
    }
}
