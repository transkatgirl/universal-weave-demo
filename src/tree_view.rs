//! Custom-painted tree/DAG visualization for the weave.

use std::collections::{HashMap, HashSet};
use std::hash::RandomState;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2,
    epaint::CubicBezierShape,
};
use universal_weave::{Node, hashbrown};
use universal_weave_layout::{
    self, Direction, LayoutConfig,
    curve::{self, CubicBezier},
    glam::Vec2 as CurvePoint,
};

const NODE_W: f32 = 170.0;
const NODE_H: f32 = 46.0;
const X_GAP: f32 = 80.0;
const Y_GAP: f32 = 30.0;
const MARGIN: f32 = 30.0;
const CURVE_FIT_TOLERANCE: f32 = 2.0;

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
    edge_curves: HashMap<(u64, u64), Vec<CubicBezier<CurvePoint>>>,
    size: Vec2,
}

/// Minimal weave node used to adapt the renderer's snapshot to the layout crate.
struct LayoutNode {
    id: u64,
    children: Vec<u64>,
}

impl Node<u64, ()> for LayoutNode {
    type From = ();
    type To = Vec<u64>;

    fn id(&self) -> u64 {
        self.id
    }

    fn from(&self) -> &Self::From {
        &()
    }

    fn to(&self) -> &Self::To {
        &self.children
    }

    fn is_active(&self) -> bool {
        false
    }

    fn contents(&self) -> &() {
        &()
    }
}

/// Computes a left-to-right Sugiyama layout and connector routes.
fn layout(ordered: &[TreeNode]) -> TreeLayout {
    let ids: HashSet<u64> = ordered.iter().map(|node| node.id).collect();
    let mut graph = hashbrown::HashMap::with_capacity_and_hasher(ordered.len(), RandomState::new());

    for node in ordered {
        graph.insert(
            node.id,
            LayoutNode {
                id: node.id,
                children: Vec::new(),
            },
        );
    }

    for node in ordered {
        for parent in node.parents.iter().filter(|parent| ids.contains(parent)) {
            graph
                .get_mut(parent)
                .expect("parent was checked against the node set")
                .children
                .push(node.id);
        }
    }

    let roots: Vec<u64> = ordered
        .iter()
        .filter(|node| node.parents.iter().all(|parent| !ids.contains(parent)))
        .map(|node| node.id)
        .collect();
    let computed = universal_weave_layout::compute::<u64, LayoutNode, (), RandomState>(
        &graph,
        roots.iter(),
        &LayoutConfig {
            node_spacing: Y_GAP,
            rank_spacing: X_GAP,
            direction: Direction::LeftToRight,
        },
        |_| [NODE_W, NODE_H].into(),
    );

    let positions = computed
        .nodes
        .iter()
        .map(|(id, node)| {
            let center = node.position + node.size / 2.0;
            (*id, Pos2::new(center.x + MARGIN, center.y + MARGIN))
        })
        .collect();

    let edge_curves = computed
        .edges
        .iter()
        .map(|(edge, points)| {
            let mut points: Vec<CurvePoint> = points
                .iter()
                .map(|point| *point + CurvePoint::splat(MARGIN))
                .collect();

            // Layout routes edges between node centers. Clip those endpoints to
            // the facing sides of the cards before fitting the visible curve.
            points[0].x += NODE_W / 2.0;
            let last = points.len() - 1;
            points[last].x -= NODE_W / 2.0;

            let curves = curve::fit_with_tangents(
                &points,
                CurvePoint::X,
                CurvePoint::NEG_X,
                CURVE_FIT_TOLERANCE,
            );
            (*edge, curves)
        })
        .collect();

    TreeLayout {
        positions,
        edge_curves,
        size: Vec2::new(
            computed.size.x + MARGIN * 2.0,
            computed.size.y + MARGIN * 2.0,
        ),
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

    let size = layout.size.max(ui.available_size());

    // Only sense clicks: drag events fall through to the surrounding ScrollArea,
    // which gives us drag-to-pan scrolling for free.
    let (response, painter) = ui.allocate_painter(size, Sense::click());
    let to_screen = |pos: Pos2| response.rect.min + pos.to_vec2();
    let curve_to_screen = |point: CurvePoint| response.rect.min + Vec2::new(point.x, point.y);

    let visuals = ui.visuals();
    let edge_stroke = Stroke::new(1.5, visuals.weak_text_color());

    // Routed edges, drawn first so nodes sit on top. In a DAG a node gets
    // one edge from each of its parents.
    for node in nodes {
        for parent in &node.parents {
            let Some(curves) = layout.edge_curves.get(&(*parent, node.id)) else {
                continue;
            };
            for curve in curves {
                painter.add(CubicBezierShape::from_points_stroke(
                    [
                        curve_to_screen(curve.start),
                        curve_to_screen(curve.start_control),
                        curve_to_screen(curve.end_control),
                        curve_to_screen(curve.end),
                    ],
                    false,
                    Color32::TRANSPARENT,
                    edge_stroke,
                ));
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
    fn universal_weave_ui_lays_out_and_curves_a_diamond() {
        let nodes = [node(0, &[]), node(1, &[0]), node(2, &[0]), node(3, &[1, 2])];

        let layout = layout(&nodes);

        assert_eq!(layout.positions.len(), nodes.len());
        assert!(layout.positions[&0].x < layout.positions[&1].x);
        assert!(layout.positions[&0].x < layout.positions[&2].x);
        assert!(layout.positions[&1].x < layout.positions[&3].x);
        assert!(layout.positions[&2].x < layout.positions[&3].x);
        assert_ne!(layout.positions[&1].y, layout.positions[&2].y);

        for edge in [(0, 1), (0, 2), (1, 3), (2, 3)] {
            let curves = layout
                .edge_curves
                .get(&edge)
                .unwrap_or_else(|| panic!("edge {edge:?} did not get a curve"));
            assert!(!curves.is_empty());

            let source = layout.positions[&edge.0];
            let target = layout.positions[&edge.1];
            assert_eq!(
                curves[0].start,
                CurvePoint::new(source.x + NODE_W / 2.0, source.y)
            );
            assert_eq!(
                curves[curves.len() - 1].end,
                CurvePoint::new(target.x - NODE_W / 2.0, target.y)
            );
            for segments in curves.windows(2) {
                assert_eq!(segments[0].end, segments[1].start);
            }
        }
    }
}
