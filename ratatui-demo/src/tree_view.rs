//! Terminal-canvas rendering for dependent trees and independent DAGs.

use std::collections::{HashMap, HashSet};
use std::hash::RandomState;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine, Rectangle};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use universal_weave::glam::Vec2;
use universal_weave::layout::{Spacing, TopologicalLayouter};
use universal_weave::tinyvec::ArrayVec;
use universal_weave::{LayoutItem, Layouter, Node, Weave};

const NODE_WIDTH: f32 = 24.0;
const NODE_HEIGHT: f32 = 5.0;
const NODE_TEXT_PADDING: f32 = 1.0;
const MARGIN: f32 = 4.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub id: u64,
    pub parents: Vec<u64>,
    pub contents: String,
    pub bookmarked: bool,
}

#[derive(Debug, Clone)]
struct EdgeRoute {
    from: u64,
    to: u64,
    points: Vec<Vec2>,
}

#[derive(Debug, Clone, Default)]
pub struct TreeLayout {
    positions: HashMap<u64, Vec2>,
    edges: Vec<EdgeRoute>,
    size: Vec2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationDirection {
    Left,
    Down,
    Up,
    Right,
}

impl TreeLayout {
    pub fn node_center(&self, id: u64) -> Option<[f64; 2]> {
        self.positions
            .get(&id)
            .map(|point| [f64::from(point.x), f64::from(point.y)])
    }

    pub fn size(&self) -> [f64; 2] {
        [
            f64::from(self.size.x.max(1.0)),
            f64::from(self.size.y.max(1.0)),
        ]
    }

    /// Find the closest node whose center lies in the requested half-plane.
    pub fn directional_neighbor(&self, id: u64, direction: NavigationDirection) -> Option<u64> {
        let origin = self.positions.get(&id)?;
        self.positions
            .iter()
            .filter_map(|(&candidate_id, candidate)| {
                let delta = *candidate - *origin;
                let is_in_direction = match direction {
                    NavigationDirection::Left => delta.x < 0.0,
                    NavigationDirection::Down => delta.y < 0.0,
                    NavigationDirection::Up => delta.y > 0.0,
                    NavigationDirection::Right => delta.x > 0.0,
                };
                is_in_direction.then_some((candidate_id, delta.length_squared()))
            })
            .min_by(|(left_id, left_distance), (right_id, right_distance)| {
                left_distance
                    .total_cmp(right_distance)
                    .then_with(|| left_id.cmp(right_id))
            })
            .map(|(candidate_id, _)| candidate_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphViewport {
    pub center: [f64; 2],
    pub zoom: f64,
}

#[derive(Clone, Copy)]
pub struct TreeView<'a> {
    pub nodes: &'a [TreeNode],
    pub layout: &'a TreeLayout,
    pub viewport: GraphViewport,
    pub selected: Option<u64>,
    pub active: &'a HashSet<u64>,
    pub path: &'a HashSet<u64>,
    pub path_edges: &'a HashSet<(u64, u64)>,
}

/// Convert a deepest-node-to-root active path into the directed edges used by the layout.
pub fn active_path_edges(path: &[u64]) -> HashSet<(u64, u64)> {
    path.windows(2).map(|nodes| (nodes[1], nodes[0])).collect()
}

impl Default for GraphViewport {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0],
            zoom: 1.0,
        }
    }
}

impl GraphViewport {
    pub fn fit(&mut self, layout: &TreeLayout) {
        let size = layout.size();
        self.center = [size[0] / 2.0, size[1] / 2.0];
        self.zoom = 1.0;
    }

    pub fn focus(&mut self, layout: &TreeLayout, id: u64) -> bool {
        let Some(center) = layout.node_center(id) else {
            return false;
        };
        self.center = center;
        true
    }

    pub fn pan(&mut self, layout: &TreeLayout, dx: f64, dy: f64) {
        let [width, height] = self.visible_size(layout);
        self.center[0] += width * dx;
        self.center[1] += height * dy;
    }

    pub fn zoom_by(&mut self, factor: f64) {
        self.zoom = (self.zoom * factor).clamp(0.5, 8.0);
    }

    pub fn bounds(&self, layout: &TreeLayout) -> ([f64; 2], [f64; 2]) {
        let [width, height] = self.visible_size(layout);
        (
            [self.center[0] - width / 2.0, self.center[0] + width / 2.0],
            [self.center[1] - height / 2.0, self.center[1] + height / 2.0],
        )
    }

    fn visible_size(&self, layout: &TreeLayout) -> [f64; 2] {
        let [width, height] = layout.size();
        [width / self.zoom, height / self.zoom]
    }
}

/// Compute a left-to-right layout directly from a weave's stable topological order.
pub fn layout<W, N, T>(weave: &mut W) -> TreeLayout
where
    W: Weave<u64, N, T>,
    N: Node<u64, T>,
    for<'a> &'a N::From: IntoIterator<Item = &'a u64>,
{
    let mut layouter = TopologicalLayouter::<u64, RandomState>::new(Spacing {
        node: 6.0,
        layer: 12.0,
        corridor: 3.0,
        edge: 2.0,
    });
    <TopologicalLayouter<u64, RandomState> as Layouter<
        W,
        u64,
        N,
        T,
        Vec2,
        ArrayVec<[Vec2; 6]>,
    >>::layout(
        &mut layouter,
        weave,
        |_| Vec2::new(NODE_HEIGHT, NODE_WIDTH),
    );

    let computed_size = <TopologicalLayouter<u64, RandomState> as Layouter<
        W,
        u64,
        N,
        T,
        Vec2,
        ArrayVec<[Vec2; 6]>,
    >>::size(&layouter);
    let size = Vec2::new(
        computed_size.y + MARGIN * 2.0,
        computed_size.x + MARGIN * 2.0,
    );
    let mut positions = HashMap::with_capacity(weave.len());
    let mut edges = Vec::new();

    <TopologicalLayouter<u64, RandomState> as Layouter<
        W,
        u64,
        N,
        T,
        Vec2,
        ArrayVec<[Vec2; 6]>,
    >>::view(
        &mut layouter,
        Vec2::ZERO,
        computed_size,
        |item| match item {
            LayoutItem::Node { id, center, .. } => {
                positions.insert(id, Vec2::new(center.y + MARGIN, size.y - center.x - MARGIN));
            }
            LayoutItem::Polyline { from, to, points } => {
                edges.push(EdgeRoute {
                    from,
                    to,
                    points: points
                        .iter()
                        .map(|point| Vec2::new(point.y + MARGIN, size.y - point.x - MARGIN))
                        .collect(),
                });
            }
        },
    );

    TreeLayout {
        positions,
        edges,
        size,
    }
}

pub fn render(frame: &mut Frame, area: Rect, view: TreeView<'_>) {
    let TreeView {
        nodes,
        layout,
        viewport,
        selected,
        active,
        path,
        path_edges,
    } = view;
    if nodes.is_empty() {
        frame.render_widget(
            Paragraph::new("The weave is empty. Press r to add a root node.")
                .block(Block::default().borders(Borders::ALL).title(" Weave ")),
            area,
        );
        return;
    }

    let (x_bounds, y_bounds) = viewport.bounds(layout);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Weave ")
        .title_bottom(" arrows: pan  +/-: zoom  0: fit ");
    let canvas_width = block.inner(area).width;
    let canvas = Canvas::default()
        .block(block)
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(|context| {
            for edge in &layout.edges {
                let color = if path_edges.contains(&(edge.from, edge.to)) {
                    Color::Blue
                } else {
                    Color::DarkGray
                };
                for points in edge.points.windows(2) {
                    context.draw(&CanvasLine {
                        x1: f64::from(points[0].x),
                        y1: f64::from(points[0].y),
                        x2: f64::from(points[1].x),
                        y2: f64::from(points[1].y),
                        color,
                    });
                }
            }
            context.layer();

            for node in nodes {
                let Some(center) = layout.positions.get(&node.id) else {
                    continue;
                };
                let color = if selected == Some(node.id) {
                    Color::Cyan
                } else if active.contains(&node.id) {
                    Color::Green
                } else if path.contains(&node.id) {
                    Color::Blue
                } else {
                    Color::Gray
                };
                context.draw(&Rectangle {
                    x: f64::from(center.x - NODE_WIDTH / 2.0),
                    y: f64::from(center.y - NODE_HEIGHT / 2.0),
                    width: f64::from(NODE_WIDTH),
                    height: f64::from(NODE_HEIGHT),
                    color,
                });

                let marker = if node.bookmarked { "★" } else { "" };
                let prefix = format!("#{id}{marker} ", id = node.id);
                let available_width = node_label_width(f64::from(center.x), canvas_width, x_bounds)
                    .saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
                let snippet = snippet(&node.contents, available_width);
                let label = if snippet.is_empty() {
                    prefix.trim_end().to_owned()
                } else {
                    format!("{prefix}{snippet}")
                };
                context.print(
                    f64::from(center.x - NODE_WIDTH / 2.0 + NODE_TEXT_PADDING),
                    f64::from(center.y),
                    TextLine::from(Span::styled(label, Style::default().fg(color))),
                );
            }
        });
    frame.render_widget(canvas, area);
}

fn node_label_width(center_x: f64, canvas_width: u16, x_bounds: [f64; 2]) -> usize {
    let [left, right] = x_bounds;
    if canvas_width <= 1 || right <= left {
        return 0;
    }

    let label_x = center_x - f64::from(NODE_WIDTH / 2.0 - NODE_TEXT_PADDING);
    if label_x < left || label_x > right {
        return 0;
    }
    let right_edge_x = (center_x + f64::from(NODE_WIDTH / 2.0)).min(right);
    let resolution = f64::from(canvas_width - 1);
    let column = |x: f64| ((x - left) * resolution / (right - left)) as usize;

    column(right_edge_x).saturating_sub(column(label_x))
}

fn snippet(text: &str, max_width: usize) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let text = if first_line.is_empty() {
        "(empty)"
    } else {
        first_line
    };
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }

    let content_width = max_width - UnicodeWidthChar::width('…').unwrap_or(1);
    let mut width = 0;
    let mut result: String = text
        .chars()
        .take_while(|character| {
            let character_width = UnicodeWidthChar::width(*character).unwrap_or(0);
            let fits = width + character_width <= content_width;
            if fits {
                width += character_width;
            }
            fits
        })
        .collect();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{IndependentDemoNode, IndependentDemoWeave, TextContent};
    use universal_weave::indexmap::IndexSet;

    fn graph(parents: &[&[u64]]) -> IndependentDemoWeave {
        let mut weave = IndependentDemoWeave::with_capacity(parents.len(), String::new());
        for (id, parents) in parents.iter().enumerate() {
            assert!(weave.insert(IndependentDemoNode {
                id: id as u64,
                from: parents.iter().copied().collect(),
                to: IndexSet::default(),
                active: false,
                bookmarked: false,
                contents: TextContent::default(),
            }));
        }
        weave
    }

    #[test]
    fn diamond_layout_has_all_nodes_and_edges() {
        let mut weave = graph(&[&[], &[0], &[0], &[1, 2]]);
        let layout = layout::<_, IndependentDemoNode, TextContent>(&mut weave);
        assert_eq!(layout.positions.len(), 4);
        assert_eq!(layout.edges.len(), 4);
        assert!(layout.positions[&0].x < layout.positions[&3].x);
        assert_ne!(layout.positions[&1].y, layout.positions[&2].y);
    }

    #[test]
    fn active_path_edges_only_include_consecutive_path_nodes() {
        let edges = active_path_edges(&[3, 2, 1, 0]);

        assert_eq!(edges, HashSet::from([(0, 1), (1, 2), (2, 3)]));
        assert!(!edges.contains(&(0, 2)));
        assert!(!edges.contains(&(1, 3)));
    }

    #[test]
    fn node_label_width_tracks_terminal_width_and_zoom() {
        let bounds = [0.0, 100.0];
        let narrow = node_label_width(50.0, 51, bounds);
        let wide = node_label_width(50.0, 101, bounds);
        let zoomed = node_label_width(50.0, 101, [25.0, 75.0]);

        assert_eq!(narrow, 12);
        assert_eq!(wide, 23);
        assert_eq!(zoomed, 46);
    }

    #[test]
    fn snippet_fits_the_available_display_width() {
        assert_eq!(snippet("abcdefgh", 5), "abcd…");
        assert_eq!(snippet("界界界", 5), "界界…");
        assert_eq!(snippet("", 4), "(em…");
        assert_eq!(snippet("text", 1), "…");
        assert_eq!(snippet("text", 0), "");
    }

    #[test]
    fn viewport_fit_pan_zoom_and_focus_are_stable() {
        let mut weave = graph(&[&[], &[0], &[0], &[1, 2]]);
        let layout = layout::<_, IndependentDemoNode, TextContent>(&mut weave);
        let mut viewport = GraphViewport::default();
        viewport.fit(&layout);
        let fitted = viewport.bounds(&layout);
        viewport.zoom_by(2.0);
        let zoomed = viewport.bounds(&layout);
        assert!(zoomed.0[1] - zoomed.0[0] < fitted.0[1] - fitted.0[0]);
        viewport.pan(&layout, 0.1, -0.1);
        assert_ne!(
            viewport.center,
            [layout.size()[0] / 2.0, layout.size()[1] / 2.0]
        );
        assert!(viewport.focus(&layout, 3));
        assert_eq!(viewport.center, layout.node_center(3).unwrap());
    }

    #[test]
    fn directional_navigation_uses_positions_and_nearest_distance() {
        let layout = TreeLayout {
            positions: HashMap::from([
                (0, Vec2::ZERO),
                (1, Vec2::new(-3.0, 0.0)),
                (2, Vec2::new(2.0, 0.0)),
                (3, Vec2::new(0.0, -4.0)),
                (4, Vec2::new(0.0, 5.0)),
                // In the right half-plane, but farther away than #2.
                (5, Vec2::new(1.0, 20.0)),
            ]),
            ..TreeLayout::default()
        };

        assert_eq!(
            layout.directional_neighbor(0, NavigationDirection::Left),
            Some(1)
        );
        assert_eq!(
            layout.directional_neighbor(0, NavigationDirection::Down),
            Some(3)
        );
        assert_eq!(
            layout.directional_neighbor(0, NavigationDirection::Up),
            Some(4)
        );
        assert_eq!(
            layout.directional_neighbor(0, NavigationDirection::Right),
            Some(2)
        );
    }

    #[test]
    fn directional_navigation_does_not_wrap() {
        let layout = TreeLayout {
            positions: HashMap::from([(0, Vec2::ZERO), (1, Vec2::new(2.0, 0.0))]),
            ..TreeLayout::default()
        };

        assert_eq!(
            layout.directional_neighbor(0, NavigationDirection::Left),
            None
        );
        assert_eq!(
            layout.directional_neighbor(1, NavigationDirection::Right),
            None
        );
    }
}
