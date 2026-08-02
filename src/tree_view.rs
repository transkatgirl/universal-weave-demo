//! Custom-painted tree/DAG visualization for the weave.

use std::collections::{HashMap, HashSet};

use dagre::graph::Graph;
use dagre::{EdgeLabel, LayoutOptions, NodeLabel, RankDir};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

const NODE_W: f32 = 170.0;
const NODE_H: f32 = 46.0;
const X_GAP: f32 = 60.0;
const Y_GAP: f32 = 18.0;
const MARGIN: f32 = 24.0;

/// A weave-agnostic snapshot of a node, used for rendering.
pub struct TreeNode {
    pub id: u64,
    pub parents: Vec<u64>,
    pub contents: String,
    pub bookmarked: bool,
}

/// How the user interacted with the tree this frame.
#[derive(Default)]
pub struct TreeResponse {
    /// A node that was single-clicked.
    pub clicked: Option<u64>,
    /// A node that was double-clicked.
    pub double_clicked: Option<u64>,
}

struct TreeLayout {
    positions: HashMap<u64, Pos2>,
    edge_paths: HashMap<(u64, u64), Vec<Pos2>>,
}

/// Computes a left-to-right Sugiyama layout and connector routes with Dagre.
fn layout(ordered: &[TreeNode]) -> TreeLayout {
    let ids: HashSet<u64> = ordered.iter().map(|node| node.id).collect();
    let mut graph = Graph::<NodeLabel, EdgeLabel>::new();

    for node in ordered {
        graph.set_node(
            node.id.to_string(),
            Some(NodeLabel {
                width: f64::from(NODE_W),
                height: f64::from(NODE_H),
                ..NodeLabel::default()
            }),
        );
    }

    for node in ordered {
        for parent in node.parents.iter().filter(|parent| ids.contains(parent)) {
            graph.set_edge(
                parent.to_string(),
                node.id.to_string(),
                Some(EdgeLabel::default()),
                None,
            );
        }
    }

    dagre::layout(
        &mut graph,
        Some(LayoutOptions {
            rankdir: RankDir::LR,
            align: Some(dagre::Align::UL),
            ranker: dagre::Ranker::NetworkSimplex,
            nodesep: f64::from(Y_GAP),
            ranksep: f64::from(X_GAP),
            marginx: f64::from(MARGIN),
            marginy: f64::from(MARGIN),
            ..LayoutOptions::default()
        }),
    );

    let positions = ordered
        .iter()
        .filter_map(|node| {
            let label = graph.node(&node.id.to_string())?;
            Some((node.id, Pos2::new(label.x? as f32, label.y? as f32)))
        })
        .collect();

    let mut edge_paths = HashMap::new();
    for node in ordered {
        for parent in node.parents.iter().filter(|parent| ids.contains(parent)) {
            let Some(edge) = graph.edge(&parent.to_string(), &node.id.to_string(), None) else {
                continue;
            };
            edge_paths.insert(
                (*parent, node.id),
                edge.points
                    .iter()
                    .map(|point| Pos2::new(point.x as f32, point.y as f32))
                    .collect(),
            );
        }
    }

    TreeLayout {
        positions,
        edge_paths,
    }
}

fn snippet(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let mut taken: String = first_line.chars().take(max_chars).collect();
    if first_line.chars().count() > max_chars {
        taken.push('…');
    }
    taken
}

/// Renders the weave into the current `ui` (intended to be inside a `ScrollArea`).
///
/// `active` is the set of active nodes (a single node for dependent weaves, the whole
/// active path for independent weaves); `path` is the ordered active path used for the
/// fill tint.
pub fn show(
    ui: &mut egui::Ui,
    nodes: &[TreeNode],
    selected: Option<u64>,
    active: &HashSet<u64>,
    path: &HashSet<u64>,
) -> TreeResponse {
    let mut result = TreeResponse::default();

    if nodes.is_empty() {
        ui.label("The weave is empty — add a root node from the toolbar.");
        return result;
    }

    let layout = layout(nodes);

    let (mut max_x, mut max_y) = (0.0f32, 0.0f32);
    for pos in layout.positions.values() {
        max_x = max_x.max(pos.x);
        max_y = max_y.max(pos.y);
    }
    let size = Vec2::new(max_x + NODE_W / 2.0 + MARGIN, max_y + NODE_H / 2.0 + MARGIN)
        .max(ui.available_size());

    // Only sense clicks: drag events fall through to the surrounding ScrollArea,
    // which gives us drag-to-pan scrolling for free.
    let (response, painter) = ui.allocate_painter(size, Sense::click());
    let to_screen = |pos: Pos2| response.rect.min + pos.to_vec2();

    let visuals = ui.visuals();
    let edge_stroke = Stroke::new(1.5, visuals.weak_text_color());

    // Dagre-routed edges, drawn first so nodes sit on top. In a DAG a node gets
    // one edge from each of its parents.
    for node in nodes {
        for parent in &node.parents {
            let Some(points) = layout.edge_paths.get(&(*parent, node.id)) else {
                continue;
            };
            for segment in points.windows(2) {
                painter.line_segment([to_screen(segment[0]), to_screen(segment[1])], edge_stroke);
            }
        }
    }

    // Nodes.
    for node in nodes {
        let Some(pos) = layout.positions.get(&node.id) else {
            continue;
        };
        let rect = Rect::from_center_size(to_screen(*pos), Vec2::new(NODE_W, NODE_H));

        let is_active = active.contains(&node.id);
        let on_path = path.contains(&node.id);
        let is_selected = selected == Some(node.id);

        let fill = if on_path {
            Color32::from_rgb(43, 62, 80)
        } else {
            visuals.widgets.inactive.bg_fill
        };
        let (border, width) = if is_active {
            (Color32::from_rgb(80, 200, 120), 2.5)
        } else if is_selected {
            (visuals.strong_text_color(), 2.0)
        } else {
            (visuals.widgets.inactive.bg_stroke.color, 1.0)
        };

        painter.rect_filled(rect, 6, fill);
        painter.rect_stroke(rect, 6, Stroke::new(width, border), StrokeKind::Inside);

        painter.text(
            rect.left_center() + Vec2::new(10.0, -9.0),
            Align2::LEFT_CENTER,
            snippet(&node.contents, 22),
            FontId::proportional(13.0),
            visuals.text_color(),
        );
        painter.text(
            rect.left_center() + Vec2::new(10.0, 11.0),
            Align2::LEFT_CENTER,
            format!("#{}", node.id),
            FontId::monospace(11.0),
            visuals.weak_text_color(),
        );

        if node.bookmarked {
            painter.circle_filled(
                rect.right_top() + Vec2::new(-10.0, 10.0),
                4.5,
                Color32::GOLD,
            );
        }
    }

    // Interaction.
    let node_at = |point: Pos2| {
        nodes.iter().find_map(|node| {
            let pos = layout.positions.get(&node.id)?;
            let rect = Rect::from_center_size(to_screen(*pos), Vec2::new(NODE_W, NODE_H));
            rect.contains(point).then_some(node.id)
        })
    };

    if response.double_clicked()
        && let Some(point) = response.interact_pointer_pos()
    {
        result.double_clicked = node_at(point);
    } else if response.clicked()
        && let Some(point) = response.interact_pointer_pos()
    {
        result.clicked = node_at(point);
    }

    if let Some(hovered) = response.hover_pos().and_then(node_at)
        && let Some(node) = nodes.iter().find(|node| node.id == hovered)
    {
        let preview = snippet(&node.contents, 400);
        response.on_hover_ui(|ui| {
            ui.label(format!("#{hovered}"));
            if preview.is_empty() {
                ui.weak("(empty node)");
            } else {
                ui.label(preview);
            }
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u64, parents: &[u64]) -> TreeNode {
        TreeNode {
            id,
            parents: parents.to_vec(),
            contents: String::new(),
            bookmarked: false,
        }
    }

    #[test]
    fn dagre_lays_out_and_routes_a_diamond() {
        let nodes = [node(0, &[]), node(1, &[0]), node(2, &[0]), node(3, &[1, 2])];

        let layout = layout(&nodes);

        assert_eq!(layout.positions.len(), nodes.len());
        assert!(layout.positions[&0].x < layout.positions[&1].x);
        assert!(layout.positions[&0].x < layout.positions[&2].x);
        assert!(layout.positions[&1].x < layout.positions[&3].x);
        assert!(layout.positions[&2].x < layout.positions[&3].x);
        assert_ne!(layout.positions[&1].y, layout.positions[&2].y);

        for edge in [(0, 1), (0, 2), (1, 3), (2, 3)] {
            assert!(
                layout
                    .edge_paths
                    .get(&edge)
                    .is_some_and(|path| path.len() >= 2),
                "edge {edge:?} did not get a Dagre route"
            );
        }
    }
}
