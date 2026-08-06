//! Custom-painted 3D radial visualization of the weave.
//!
//! [`universal_weave_layout::compute_3d`] arranges the weave as a stack of
//! rank circles around the `+y` axis: every rank becomes one ring whose radius
//! is emergent, sized so that the rank's cards exactly fill its circumference
//! with a single seam gap, so sparse ranks hug the axis and dense ranks open
//! out. This module projects that geometry to the screen in software and draws
//! it with egui's ordinary 2D painter: node cards are billboards that always
//! face the camera (so their text stays readable and hit-testing stays a
//! rectangle test), and edges are flattened into screen-space line segments.
//! Cards and edge segments share one depth-sorted draw list, so connectors
//! correctly weave in front of and behind the cards they pass.
//!
//! Because the cards are billboards rather than tangents to their ring, a ring
//! whose radius is small relative to the card width projects its cards on top
//! of one another; the depth sort keeps the nearer card whole, and orbiting
//! separates them.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
use std::hash::RandomState;

use eframe::egui::{
    self, Align2, Color32, FontId, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2,
};
use universal_weave_layout::{self, EdgeEndpoints, Layout3Config, NodeLayout3, curve, glam::Vec3};

use crate::tree_view::{LayoutNode, TreeNode, TreeResponse, build_graph};

const NODE_W: f32 = 170.0;
const NODE_H: f32 = 46.0;
/// Cards are billboards with no real thickness. Depth no longer enters the
/// layout at all — the width alone is the card's circumferential footprint —
/// so it is only passed through for the renderer, which does not use it.
const NODE_D: f32 = 0.0;
/// Minimum arc-length gap between the borders of adjacent cards on a ring, the
/// seam between the first and last card included. Ring radii are emergent —
/// each rank's is whatever makes the rank exactly fill its circumference — so
/// this is the only lever on how far the rings open.
const NODE_SPACING: f32 = 30.0;
/// Minimum arc-length gap kept around the corridor an edge skipping ranks
/// reserves on each ring it crosses. As in the 2D view the corridors
/// themselves are widthless, so this is the only margin between a connector
/// and the cards it passes; zero lets it run along their borders.
const EDGE_GAP: f32 = 0.0;
const RANK_SPACING: f32 = 130.0;
/// How far the smoothed connectors' control arms reach along the rank axis, as
/// a fraction of half the segment's axial span. `1.0` is the roundest.
const CURVE_ROUNDNESS: f32 = 1.0;
const FLATTEN_TOLERANCE: f32 = 1.0;

const FOV_Y: f32 = FRAC_PI_4;
const NEAR: f32 = 1.0;
/// Keeps the view direction off the `+y` axis, where the basis would collapse.
const PITCH_LIMIT: f32 = FRAC_PI_2 - 0.05;
const DEFAULT_YAW: f32 = 0.6;
const DEFAULT_PITCH: f32 = 0.35;
const ORBIT_SPEED: f32 = 0.008;
const ZOOM_SPEED: f32 = 0.0015;
const MIN_DISTANCE: f32 = 60.0;
const MAX_DISTANCE: f32 = 200_000.0;
/// The bounding sphere frames the box diagonal, but the geometry inside that
/// box is a stack of rings rather than a solid, so the framing leaves a wide
/// margin; pull the camera in to compensate.
const FIT_MARGIN: f32 = 0.8;
/// Labels stop shrinking with the cards at this point size, so distant nodes
/// stay readable; the text is truncated to whatever still fits instead.
const MIN_LABEL_PT: f32 = 7.5;
/// How far the most distant geometry is faded toward the panel background.
const FADE_STRENGTH: f32 = 0.75;

/// An orbit camera looking at a point on the rank axis.
pub struct Camera {
    yaw: f32,
    pitch: f32,
    distance: f32,
    target: Vec3,
    fitted: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            distance: 800.0,
            target: Vec3::ZERO,
            fitted: false,
        }
    }
}

impl Camera {
    /// Frames the whole layout and resets the orbit angles.
    fn fit(&mut self, size: Vec3) {
        self.yaw = DEFAULT_YAW;
        self.pitch = DEFAULT_PITCH;
        // The layout's box spans (-x/2, 0, -z/2) to (x/2, y, z/2); `upright`
        // reflects it below the origin, so its center sits on the axis at
        // minus half its height.
        self.target = Vec3::new(0.0, -size.y / 2.0, 0.0);
        let radius = (size * 0.5).length().max(NODE_W);
        self.distance =
            (radius * FIT_MARGIN / (FOV_Y * 0.5).tan()).clamp(MIN_DISTANCE, MAX_DISTANCE);
        self.fitted = true;
    }

    /// Requests that the next frame re-frame the layout.
    pub fn reset(&mut self) {
        self.fitted = false;
    }

    fn eye(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        self.target + Vec3::new(cos_pitch * sin_yaw, sin_pitch, cos_pitch * cos_yaw) * self.distance
    }
}

/// The camera basis and screen mapping derived for one frame.
struct View {
    eye: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    center: Pos2,
    focal: f32,
}

impl View {
    fn new(camera: &Camera, viewport: Rect) -> Self {
        let eye = camera.eye();
        let forward = (camera.target - eye).normalize_or_zero();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward);
        Self {
            eye,
            right,
            up,
            forward,
            center: viewport.center(),
            focal: viewport.height() * 0.5 / (FOV_Y * 0.5).tan(),
        }
    }

    /// Projects a world point, returning its screen position and its depth
    /// along the view axis, or `None` if it lies at or behind the near plane.
    fn project(&self, world: Vec3) -> Option<(Pos2, f32)> {
        let delta = world - self.eye;
        let depth = delta.dot(self.forward);
        if !depth.is_finite() || depth <= NEAR {
            return None;
        }
        let scale = self.focal / depth;
        let offset = Vec2::new(delta.dot(self.right), -delta.dot(self.up)) * scale;
        Some((self.center + offset, depth))
    }
}

struct RadialLayout {
    nodes: HashMap<u64, NodeLayout3>,
    /// Edge polylines keyed by `(parent, child)`, already flattened from the
    /// smoothed Bézier path into line segments.
    edges: HashMap<(u64, u64), Vec<Vec3>>,
    size: Vec3,
}

/// Reflects a layout point onto the rendering axis.
///
/// The layout stacks ranks along `+y` with the roots at `y = 0`, which would
/// put the roots underneath. Negating `y` hangs the stack the way a tree reads
/// instead: roots at the top, later ranks below them, matching the 2D view's
/// root-first ordering.
fn upright(point: Vec3) -> Vec3 {
    Vec3::new(point.x, -point.y, point.z)
}

/// Computes the radial layered layout and its smoothed connector routes.
fn layout(ordered: &[TreeNode]) -> RadialLayout {
    let (graph, roots) = build_graph(ordered);
    let computed = universal_weave_layout::compute_3d::<u64, LayoutNode, (), RandomState>(
        &graph,
        roots.iter(),
        &Layout3Config {
            node_spacing: NODE_SPACING,
            rank_spacing: RANK_SPACING,
            // Connectors are thin lines, so their corridors need no arc width
            // of their own; reserving the position is what keeps the bend
            // points out of the cards, and `dummy_spacing` sets the margin.
            edge_spacing: 0.0,
            dummy_spacing: Some(EDGE_GAP),
            // As in the 2D view, routes start and end on the facing card
            // borders; the billboards cover the joints either way, but ending
            // at the border keeps the curve's tangents out of the cards.
            endpoints: EdgeEndpoints::Border,
        },
        |_| Vec3::new(NODE_W, NODE_H, NODE_D),
    );

    let nodes = computed
        .nodes
        .iter()
        .map(|(id, node)| {
            let mut node = *node;
            node.position = upright(node.position);
            (*id, node)
        })
        .collect();

    // Smooth and flatten in the layout's own space, where ranks advance along
    // +y, so `smooth` gets the rank axis it expects; the reflection is applied
    // to the resulting points. The endpoints sit on the cards' `y` borders,
    // which the billboards cover once the geometry is projected.
    let edges = computed
        .edges
        .iter()
        .map(|(edge, points)| {
            let path = curve::smooth(points, Vec3::Y, CURVE_ROUNDNESS);
            let polyline = curve::flatten_path(&path, FLATTEN_TOLERANCE)
                .into_iter()
                .map(upright)
                .collect();
            (*edge, polyline)
        })
        .collect();

    RadialLayout {
        nodes,
        edges,
        size: computed.size,
    }
}

/// One depth-sorted primitive.
struct Draw {
    depth: f32,
    item: Item,
}

enum Item {
    Edge {
        from: Pos2,
        to: Pos2,
        width: f32,
    },
    /// Indexes into the node snapshot the draw list was built from.
    Card {
        index: usize,
        rect: Rect,
        scale: f32,
    },
}

/// The handful of theme colors the radial view paints with.
struct Palette {
    background: Color32,
    edge: Color32,
    text: Color32,
    weak_text: Color32,
    strong_text: Color32,
    card_fill: Color32,
    card_stroke: Color32,
}

impl Palette {
    fn new(visuals: &egui::Visuals) -> Self {
        Self {
            background: visuals.panel_fill,
            edge: visuals.weak_text_color(),
            text: visuals.text_color(),
            weak_text: visuals.weak_text_color(),
            strong_text: visuals.strong_text_color(),
            card_fill: visuals.widgets.inactive.bg_fill,
            card_stroke: visuals.widgets.inactive.bg_stroke.color,
        }
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

/// Applies orbit, pan, and zoom from this frame's input.
fn interact(ui: &egui::Ui, response: &egui::Response, camera: &mut Camera) {
    if response.dragged() {
        let delta = response.drag_delta();
        let panning =
            response.dragged_by(PointerButton::Middle) || ui.input(|input| input.modifiers.shift);
        if panning {
            // Move the target in the camera plane so the scene tracks the cursor.
            let view = View::new(camera, response.rect);
            let world_per_pixel = camera.distance / view.focal;
            camera.target += (view.right * -delta.x + view.up * delta.y) * world_per_pixel;
        } else {
            camera.yaw -= delta.x * ORBIT_SPEED;
            camera.pitch = (camera.pitch + delta.y * ORBIT_SPEED).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        }
    }

    if response.hovered() {
        let (scroll, zoom) = ui.input(|input| (input.smooth_scroll_delta.y, input.zoom_delta()));
        if scroll != 0.0 || zoom != 1.0 {
            camera.distance = (camera.distance * (-scroll * ZOOM_SPEED).exp() / zoom)
                .clamp(MIN_DISTANCE, MAX_DISTANCE);
        }
    }
}

/// Projects the layout into a depth-sorted draw list, farthest primitive first.
fn build_draws(nodes: &[TreeNode], layout: &RadialLayout, view: &View) -> Vec<Draw> {
    let mut draws = Vec::new();

    for node in nodes {
        for parent in &node.parents {
            let Some(polyline) = layout.edges.get(&(*parent, node.id)) else {
                continue;
            };
            // Each segment is sorted on its own so a connector can pass both in
            // front of and behind the cards along its route.
            for pair in polyline.windows(2) {
                let (Some((from, from_depth)), Some((to, to_depth))) =
                    (view.project(pair[0]), view.project(pair[1]))
                else {
                    continue;
                };
                let depth = (from_depth + to_depth) * 0.5;
                draws.push(Draw {
                    depth,
                    item: Item::Edge {
                        from,
                        to,
                        width: (1.5 * view.focal / depth).clamp(0.5, 4.0),
                    },
                });
            }
        }
    }

    for (index, node) in nodes.iter().enumerate() {
        let Some(placement) = layout.nodes.get(&node.id) else {
            continue;
        };
        // Unlike the 2D layout, `position` is already the node's center.
        let Some((screen, depth)) = view.project(placement.position) else {
            continue;
        };
        let scale = view.focal / depth;
        draws.push(Draw {
            depth,
            item: Item::Card {
                index,
                rect: Rect::from_center_size(screen, Vec2::new(NODE_W, NODE_H) * scale),
                scale,
            },
        });
    }

    draws.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    draws
}

/// The depth range spanned by a draw list, used to normalize the distance fade.
fn depth_range(draws: &[Draw]) -> (f32, f32) {
    let mut near = f32::INFINITY;
    let mut far = f32::NEG_INFINITY;
    for draw in draws {
        near = near.min(draw.depth);
        far = far.max(draw.depth);
    }
    (near, far)
}

#[expect(clippy::too_many_arguments, reason = "a painting helper, not an API")]
fn paint_card(
    painter: &egui::Painter,
    palette: &Palette,
    node: &TreeNode,
    rect: Rect,
    scale: f32,
    fade: f32,
    is_active: bool,
    on_path: bool,
    is_selected: bool,
) {
    let fill = if on_path {
        Color32::from_rgb(43, 62, 80)
    } else {
        palette.card_fill
    };
    let (border, width) = if is_active {
        (Color32::from_rgb(80, 200, 120), 2.5)
    } else if is_selected {
        (palette.strong_text, 2.0)
    } else {
        (palette.card_stroke, 1.0)
    };

    let corner = (6.0 * scale).round().clamp(0.0, 255.0) as u8;
    painter.rect_filled(rect, corner, fill.lerp_to_gamma(palette.background, fade));
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(
            (width * scale).max(0.75),
            border.lerp_to_gamma(palette.background, fade),
        ),
        StrokeKind::Inside,
    );

    // Labels shrink with their card but stop at a readable floor, so distant
    // nodes keep a legible caption instead of blurring out. Once the point size
    // stops tracking the card, the text has to be truncated to what still fits.
    let title_pt = (13.0 * scale).max(MIN_LABEL_PT);
    let id_pt = (11.0 * scale).max(MIN_LABEL_PT - 1.0);
    let padding = (10.0 * scale).max(3.0);
    let budget = ((rect.width() - padding * 2.0) / (title_pt * 0.55)).floor();
    let budget = if budget > 0.0 { budget as usize } else { 0 };

    if budget > 0 {
        let contents = snippet(&node.contents, budget.min(22));
        // The id line only earns its space while the card is tall enough for
        // two floored lines; below that the contents alone are worth more.
        if rect.height() >= (title_pt + id_pt) * 1.35 {
            painter.text(
                rect.left_center() + Vec2::new(padding, rect.height() * -0.2),
                Align2::LEFT_CENTER,
                contents,
                FontId::proportional(title_pt),
                palette.text.lerp_to_gamma(palette.background, fade),
            );
            painter.text(
                rect.left_center() + Vec2::new(padding, rect.height() * 0.24),
                Align2::LEFT_CENTER,
                format!("#{}", node.id),
                FontId::monospace(id_pt),
                palette.weak_text.lerp_to_gamma(palette.background, fade),
            );
        } else {
            painter.text(
                rect.left_center() + Vec2::new(padding, 0.0),
                Align2::LEFT_CENTER,
                contents,
                FontId::proportional(title_pt),
                palette.text.lerp_to_gamma(palette.background, fade),
            );
        }
    }

    if node.bookmarked {
        painter.circle_filled(
            rect.right_top() + Vec2::new(-10.0, 10.0) * scale,
            (4.5 * scale).max(1.5),
            Color32::GOLD.lerp_to_gamma(palette.background, fade),
        );
    }
}

/// Finds the frontmost card containing `point`.
fn node_at(draws: &[Draw], nodes: &[TreeNode], point: Pos2) -> Option<u64> {
    // The list is sorted farthest-first, so walking it backwards visits the
    // nearest card first.
    draws.iter().rev().find_map(|draw| match draw.item {
        Item::Card { index, rect, .. } if rect.contains(point) => Some(nodes[index].id),
        _ => None,
    })
}

/// Renders the weave as a stack of rings into the current `ui`, orbiting
/// `camera`.
///
/// Takes the whole available space and senses drags, so it must not be placed
/// inside a `ScrollArea`. `active` and `path` carry the same meaning as in
/// [`crate::tree_view::show`].
pub fn show(
    ui: &mut egui::Ui,
    nodes: &[TreeNode],
    selected: Option<u64>,
    active: &std::collections::HashSet<u64>,
    path: &std::collections::HashSet<u64>,
    camera: &mut Camera,
) -> TreeResponse {
    let mut result = TreeResponse::default();

    if nodes.is_empty() {
        ui.label("The weave is empty — add a root node from the toolbar.");
        return result;
    }

    let layout = layout(nodes);
    if !camera.fitted {
        camera.fit(layout.size);
    }

    let palette = Palette::new(ui.visuals());
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());

    // Apply this frame's camera input before projecting, so the view responds
    // on the same frame as the drag.
    interact(ui, &response, camera);

    let view = View::new(camera, response.rect);
    let draws = build_draws(nodes, &layout, &view);
    let (near, far) = depth_range(&draws);
    let fade_of = |depth: f32| {
        if far > near {
            (depth - near) / (far - near) * FADE_STRENGTH
        } else {
            0.0
        }
    };

    for draw in &draws {
        let fade = fade_of(draw.depth);
        match draw.item {
            Item::Edge { from, to, width } => {
                painter.line_segment(
                    [from, to],
                    Stroke::new(width, palette.edge.lerp_to_gamma(palette.background, fade)),
                );
            }
            Item::Card { index, rect, scale } => {
                let node = &nodes[index];
                paint_card(
                    &painter,
                    &palette,
                    node,
                    rect,
                    scale,
                    fade,
                    active.contains(&node.id),
                    path.contains(&node.id),
                    selected == Some(node.id),
                );
            }
        }
    }

    if response.double_clicked()
        && let Some(point) = response.interact_pointer_pos()
    {
        result.double_clicked = node_at(&draws, nodes, point);
    } else if response.clicked()
        && let Some(point) = response.interact_pointer_pos()
    {
        result.clicked = node_at(&draws, nodes, point);
    }

    if let Some(hovered) = response
        .hover_pos()
        .and_then(|point| node_at(&draws, nodes, point))
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

    fn diamond() -> [TreeNode; 4] {
        [node(0, &[]), node(1, &[0]), node(2, &[0]), node(3, &[1, 2])]
    }

    /// The horizontal distance of a placed card from the rank axis.
    fn radius_of(point: Vec3) -> f32 {
        point.x.hypot(point.z)
    }

    #[test]
    fn radial_layout_hangs_ranks_below_the_root() {
        let nodes = diamond();

        let layout = layout(&nodes);

        assert_eq!(layout.nodes.len(), nodes.len());

        let placement = |id: u64| layout.nodes[&id];
        assert_eq!(placement(0).rank, 0);
        assert_eq!(placement(1).rank, 1);
        assert_eq!(placement(2).rank, 1);
        assert_eq!(placement(3).rank, 2);

        // `upright` reflects the rank axis, so deeper ranks hang below the
        // root; the two middle nodes still share a band.
        assert!(placement(0).position.y > placement(1).position.y);
        assert!(placement(1).position.y > placement(3).position.y);
        assert_eq!(placement(1).position.y, placement(2).position.y);
        assert!(placement(3).position.y < 0.0);

        // Every rank is a ring, so `radius` is the whole rank's, and even a
        // lone root sits out on its own ring rather than on the axis.
        for id in 0..4 {
            let placed = placement(id);
            assert!(placed.radius > 0.0);
            assert!((radius_of(placed.position) - placed.radius).abs() < 0.01);
        }

        // Siblings share their rank's ring, which has to open wide enough to
        // fit them both around it, but sit at different angles on it.
        assert_eq!(placement(1).radius, placement(2).radius);
        assert_ne!(placement(1).angle, placement(2).angle);
        assert!(placement(1).radius > placement(0).radius);

        // Ranks 0 and 2 hold one card of the same width, so their rings are
        // the same size and their single cards line up angularly.
        assert_eq!(placement(3).radius, placement(0).radius);
        assert_eq!(placement(3).angle, placement(0).angle);

        for edge in [(0, 1), (0, 2), (1, 3), (2, 3)] {
            let polyline = layout
                .edges
                .get(&edge)
                .unwrap_or_else(|| panic!("edge {edge:?} did not get a polyline"));
            assert!(polyline.len() >= 2, "edge {edge:?} was not a drawable line");
            assert!(polyline.iter().all(|point| point.is_finite()));
        }
    }

    #[test]
    fn projection_centers_the_target() {
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let camera = Camera {
            target: Vec3::new(0.0, 100.0, 0.0),
            distance: 1000.0,
            ..Camera::default()
        };
        let view = View::new(&camera, viewport);

        let (screen, depth) = view.project(camera.target).expect("target is in front");
        assert!((screen - viewport.center()).length() < 0.01);
        assert!((depth - camera.distance).abs() < 0.01);

        // Twice as far along the view axis projects at half the scale.
        let offset = view.right * 50.0;
        let (near, _) = view.project(camera.target + offset).expect("in front");
        let (far, _) = view
            .project(camera.target + view.forward * camera.distance + offset)
            .expect("in front");
        let near_extent = (near - viewport.center()).length();
        let far_extent = (far - viewport.center()).length();
        assert!((near_extent / far_extent - 2.0).abs() < 0.01);

        // Behind the camera there is nothing to draw.
        assert!(view.project(view.eye - view.forward * 10.0).is_none());
    }

    #[test]
    fn hit_test_prefers_the_nearest_card() {
        let nodes = diamond();
        let rect = Rect::from_center_size(Pos2::new(100.0, 100.0), Vec2::new(80.0, 40.0));
        // Sorted farthest-first, as `build_draws` leaves it.
        let draws = vec![
            Draw {
                depth: 900.0,
                item: Item::Card {
                    index: 2,
                    rect,
                    scale: 1.0,
                },
            },
            Draw {
                depth: 100.0,
                item: Item::Card {
                    index: 1,
                    rect,
                    scale: 1.0,
                },
            },
        ];

        assert_eq!(node_at(&draws, &nodes, rect.center()), Some(nodes[1].id));
        assert_eq!(node_at(&draws, &nodes, Pos2::new(500.0, 500.0)), None);
    }

    #[test]
    fn camera_fit_frames_the_layout() {
        let layout = layout(&diamond());
        let mut camera = Camera::default();
        assert!(!camera.fitted);

        camera.fit(layout.size);

        assert!(camera.fitted);
        assert_eq!(camera.target, Vec3::new(0.0, -layout.size.y / 2.0, 0.0));
        assert!(camera.distance > 0.0 && camera.distance.is_finite());

        // The camera must end up outside the geometry it is framing.
        for placement in layout.nodes.values() {
            assert!(placement.position.distance(camera.target) < camera.distance);
        }

        camera.reset();
        assert!(!camera.fitted);
    }
}
