//! Application state and the two-peer native UI.

use std::collections::HashSet;

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};

use crate::document::{Document, SyncOutcome, WeaveKind, seeded_dependent, synchronize_pair};
use crate::{persistence, tree_view};

const PEER_B_VIEWPORT: &str = "collaborative_peer_b";

/// Applies a staged active-path edit while the reading-view editor has keyboard focus.
const APPLY_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);

/// Whether a staged text buffer still applies to the document state it was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BufferStatus {
    /// The buffer matches the document.
    Clean,
    /// The buffer holds typing that can be applied to the state it was staged on.
    Staged,
    /// The document changed under staged typing, so the edit no longer applies.
    Stale,
}

/// Formats a quantity with its noun, such as "1 change" or "3 nodes".
fn count(quantity: usize, noun: &str) -> String {
    let suffix = if quantity == 1 { "" } else { "s" };
    format!("{quantity} {noun}{suffix}")
}

/// Shows whether a text buffer holds staged typing, and warns when that typing is stale.
fn buffer_status_label(
    ui: &mut egui::Ui,
    status: BufferStatus,
    stale_label: &str,
    stale_hint: &str,
) {
    match status {
        BufferStatus::Clean => {
            ui.weak("No staged changes");
        }
        BufferStatus::Staged => {
            ui.label(
                RichText::new("● Staged changes")
                    .color(Color32::YELLOW)
                    .strong(),
            );
        }
        BufferStatus::Stale => {
            ui.label(
                RichText::new(format!("⚠ {stale_label}"))
                    .color(Color32::LIGHT_RED)
                    .strong(),
            )
            .on_hover_text(stale_hint);
        }
    }
}

/// A button that puts staged typing on the clipboard, so it survives a Reset.
///
/// Shown only while the buffer holds staged typing. Returns whether it was clicked.
fn copy_button(ui: &mut egui::Ui, status: BufferStatus, text: &str) -> bool {
    if status == BufferStatus::Clean {
        return false;
    }
    let clicked = ui
        .button("Copy staged text")
        .on_hover_text("Copy the staged text to the clipboard.")
        .clicked();
    if clicked {
        ui.ctx().copy_text(text.to_owned());
    }
    clicked
}

#[derive(Default)]
struct EditorOutput {
    open: bool,
    save: bool,
    create: bool,
    reopen_peer_b: bool,
}

/// State which must remain independent for each editor/peer.
struct EditorState {
    document: Document,
    next_id: u64,
    id_step: u64,
    selected: Option<u64>,
    edit_buffer: String,
    edit_original: String,
    edit_for: Option<u64>,
    reading_buffer: String,
    reading_original_text: String,
    reading_original_path: Vec<u64>,
    split_index: usize,
    title_buffer: String,
    move_buffer: String,
    status: String,
}

impl EditorState {
    fn new(mut document: Document, next_id: u64, id_step: u64, status: String) -> Self {
        let title_buffer = document.metadata().clone();
        let (reading_original_path, reading_original_text) = document.active_path_text();
        let reading_buffer = reading_original_text.clone();
        let selected = None;
        Self {
            document,
            next_id,
            id_step,
            selected,
            edit_buffer: String::new(),
            edit_original: String::new(),
            edit_for: None,
            reading_buffer,
            reading_original_text,
            reading_original_path,
            split_index: 0,
            title_buffer,
            move_buffer: String::new(),
            status,
        }
    }

    /// Whether the inspector holds typing for the selected node that can still be applied.
    ///
    /// The buffer is compared with the contents it was loaded from, so a node that
    /// changed underneath staged typing, for example because a path edit split it or a
    /// peer edited it, is reported as stale instead of being silently overwritten.
    fn edit_status(&self) -> BufferStatus {
        let Some(id) = self.edit_for.filter(|id| Some(*id) == self.selected) else {
            return BufferStatus::Clean;
        };
        if self.edit_buffer == self.edit_original {
            BufferStatus::Clean
        } else if self.document.node_contents(&id).as_deref() == Some(self.edit_original.as_str()) {
            BufferStatus::Staged
        } else {
            BufferStatus::Stale
        }
    }

    fn edit_is_dirty(&self) -> bool {
        self.edit_status() != BufferStatus::Clean
    }

    fn title_is_dirty(&self) -> bool {
        self.title_buffer != *self.document.metadata()
    }

    fn reading_is_dirty(&self) -> bool {
        self.reading_buffer != self.reading_original_text
    }

    /// Refreshes a clean reading buffer and reports whether staged typing still applies.
    ///
    /// A dirty buffer keeps its snapshot, so typing survives structural operations and
    /// the editor can show that the path changed underneath it.
    fn sync_reading_buffer(&mut self) -> BufferStatus {
        if self.document.kind() != WeaveKind::Independent {
            return BufferStatus::Clean;
        }
        let (path, text) = self.document.active_path_text();
        let path_changed = path != self.reading_original_path || text != self.reading_original_text;
        match (self.reading_is_dirty(), path_changed) {
            (true, true) => BufferStatus::Stale,
            (true, false) => BufferStatus::Staged,
            (false, changed) => {
                if changed {
                    self.reading_original_path = path;
                    self.reading_original_text = text.clone();
                    self.reading_buffer = text;
                }
                BufferStatus::Clean
            }
        }
    }

    fn reset_reading_buffer(&mut self) {
        let (path, text) = self.document.active_path_text();
        self.reading_original_path = path;
        self.reading_original_text = text.clone();
        self.reading_buffer = text;
        self.status = "Reset active-path edit buffer".to_string();
    }

    /// Makes the path a stale edit was staged on active again, so the edit applies.
    fn restore_reading_path(&mut self) {
        if let Err(error) = self.document.restore_path(&self.reading_original_path) {
            self.status = error;
            return;
        }
        self.status = match self.sync_reading_buffer() {
            BufferStatus::Staged => {
                "Restored the active path; the staged edit can be applied".to_string()
            }
            BufferStatus::Stale => {
                "Restored the active path, but its text changed; reset the buffer before applying"
                    .to_string()
            }
            BufferStatus::Clean => "Restored the active path".to_string(),
        };
    }

    fn apply_reading_buffer(&mut self) {
        match self.document.replace_active_path_text(
            &self.reading_original_path,
            &self.reading_original_text,
            &self.reading_buffer,
            &mut self.next_id,
            self.id_step,
        ) {
            Ok(Some(summary)) => {
                let (path, text) = self.document.active_path_text();
                self.reading_original_path = path;
                self.reading_original_text = text.clone();
                self.reading_buffer = text;
                // Show the first change in the inspector, unless that would discard typing there.
                if let Some(id) = summary.focus
                    && !self.edit_is_dirty()
                {
                    self.selected = Some(id);
                    self.edit_for = None;
                }
                let mut details = Vec::new();
                if summary.created_nodes > 0 {
                    details.push(format!("creating {}", count(summary.created_nodes, "node")));
                }
                if summary.reused_branches > 0 {
                    let noun = if summary.reused_branches == 1 {
                        "existing branch"
                    } else {
                        "existing branches"
                    };
                    details.push(format!("switching to {} {noun}", summary.reused_branches));
                }
                let details = if details.is_empty() {
                    String::new()
                } else {
                    format!(", {}", details.join(" and "))
                };
                self.status = format!(
                    "Applied {} to the active path{details}; replaced text remains on alternate branches",
                    count(summary.hunks, "change")
                );
            }
            Ok(None) => self.status = "Active-path edit is unchanged".to_string(),
            Err(error) => self.status = error,
        }
    }

    /// Typed input that has not been applied to the document yet.
    fn unapplied_edits(&self) -> Vec<String> {
        let mut pending = Vec::new();
        if self.reading_is_dirty() {
            pending.push("a staged active-path edit".to_string());
        }
        if self.edit_is_dirty()
            && let Some(id) = self.edit_for
        {
            pending.push(format!("an unapplied edit to node #{id}"));
        }
        pending
    }

    /// Refreshes imported state without overwriting locally typed, unapplied text.
    fn refresh_after_import(&mut self, title_was_dirty: bool) {
        if self.selected.is_some_and(|id| !self.document.contains(&id)) {
            self.selected = None;
        }
        self.sync_edit_buffer();
        if !title_was_dirty {
            self.title_buffer.clone_from(self.document.metadata());
        }
    }

    fn advance_id(&mut self) {
        self.next_id = self.next_id.saturating_add(self.id_step);
    }

    /// Loads the selected node into the inspector buffer.
    ///
    /// A clean buffer follows the node's contents when they change underneath it, such
    /// as after a path edit splits the node. A dirty buffer keeps its snapshot, so
    /// typing survives and the inspector can show that the node changed.
    fn sync_edit_buffer(&mut self) {
        if self.edit_for != self.selected {
            self.edit_original = self
                .selected
                .and_then(|id| self.document.node_contents(&id))
                .unwrap_or_default();
            self.edit_buffer.clone_from(&self.edit_original);
            self.edit_for = self.selected;
            self.split_index = self.edit_buffer.len() / 2;
            self.move_buffer.clear();
        } else if self.edit_buffer == self.edit_original
            && let Some(contents) = self
                .edit_for
                .and_then(|id| self.document.node_contents(&id))
            && contents != self.edit_original
        {
            self.edit_original = contents;
            self.edit_buffer.clone_from(&self.edit_original);
            self.split_index = self.edit_buffer.len() / 2;
        }
    }

    /// Applies the inspector buffer to its node, refusing when the node changed underneath it.
    fn apply_edit_buffer(&mut self, id: u64) {
        match self.edit_status() {
            BufferStatus::Stale => {
                self.status = format!(
                    "Node #{id} changed while this edit was staged; reset the buffer before applying"
                );
            }
            BufferStatus::Clean => self.status = format!("Contents of #{id} are unchanged"),
            BufferStatus::Staged => {
                if self.document.apply_edit(&id, self.edit_buffer.clone()) {
                    self.edit_original.clone_from(&self.edit_buffer);
                    self.status = format!("Edited contents of #{id}");
                } else {
                    self.status = format!("Failed to edit #{id}");
                }
            }
        }
    }

    fn reset_edit_buffer(&mut self) {
        self.edit_original = self
            .edit_for
            .and_then(|id| self.document.node_contents(&id))
            .unwrap_or_default();
        self.edit_buffer.clone_from(&self.edit_original);
        self.status = "Reset node edit buffer".to_string();
    }

    fn add_root(&mut self) {
        let id = self.next_id;
        if self.document.add_root(id) {
            self.advance_id();
            self.selected = Some(id);
            self.edit_for = None;
            self.status = format!("Added root node #{id}");
        } else {
            self.status = format!("Failed to add root node #{id}");
        }
    }

    fn add_child(&mut self, parent: u64) {
        let id = self.next_id;
        if self.document.add_child(&parent, id) {
            self.advance_id();
            self.selected = Some(id);
            self.edit_for = None;
            self.status = format!("Added child node #{id} under #{parent}");
        } else {
            self.status = format!("Failed to add child under #{parent}");
        }
    }

    fn split_selected(&mut self, id: u64) {
        let at = self.split_index;
        let new_id = self.next_id;
        if self.document.split(&id, at, new_id) {
            self.advance_id();
            self.edit_for = None;
            self.status = format!("Split node #{id} at byte {at}; tail became #{new_id}");
        } else {
            self.status = format!("Could not split #{id} at byte {at}");
        }
    }

    fn merge_selected(&mut self, id: u64) {
        match self.document.merge_with_parent(&id) {
            Some(parent) => {
                self.selected = Some(parent);
                self.edit_for = None;
                self.status = format!("Merged node #{id} into its parent #{parent}");
            }
            None => self.status = format!("Could not merge #{id}"),
        }
    }

    fn move_selected(&mut self, id: u64) {
        let parsed: Result<Vec<u64>, _> = self
            .move_buffer
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::parse::<u64>)
            .collect();
        match parsed {
            Ok(parents) => match self.document.move_node(&id, &parents) {
                Ok(()) => {
                    self.status = format!("Moved node #{id} under {parents:?}");
                    self.move_buffer.clear();
                }
                Err(error) => self.status = error,
            },
            Err(_) => self.status = format!("Invalid parent list: {:?}", self.move_buffer),
        }
    }

    fn delete_selected(&mut self, id: u64) {
        match self.document.remove(&id) {
            Some(removed) => {
                self.selected = None;
                self.edit_for = None;
                self.status = format!("Removed {removed} node(s)");
            }
            None => self.status = format!("Failed to remove node #{id}"),
        }
    }

    fn connection_control(ui: &mut egui::Ui, connected: &mut bool) {
        let label = if *connected { "Connected" } else { "Offline" };
        ui.toggle_value(connected, label);
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        peer_name: &str,
        connected: &mut bool,
        primary: bool,
        peer_b_open: bool,
        new_kind: &mut WeaveKind,
    ) -> EditorOutput {
        let mut output = EditorOutput::default();
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading(format!("Universal Weave Demo — {peer_name}"));
                ui.separator();

                if self.document.kind() == WeaveKind::DependentLoro {
                    Self::connection_control(ui, connected);
                    ui.separator();
                }

                ui.label("Title:");
                let response = ui.text_edit_singleline(&mut self.title_buffer);
                if response.lost_focus() && self.title_buffer != *self.document.metadata() {
                    self.document.set_metadata(self.title_buffer.clone());
                }

                if primary {
                    ui.separator();
                    output.open = ui.button("Open…").clicked();
                    output.save = ui.button("Save…").clicked();
                    ui.separator();
                    ui.label("New:");
                    egui::ComboBox::from_id_salt("new_weave_kind")
                        .selected_text(new_kind.label())
                        .show_ui(ui, |ui| {
                            for kind in [
                                WeaveKind::Dependent,
                                WeaveKind::Independent,
                                WeaveKind::DependentLoro,
                            ] {
                                ui.selectable_value(new_kind, kind, kind.label());
                            }
                        });
                    output.create = ui.button("Create").clicked();
                    if self.document.kind() == WeaveKind::DependentLoro && !peer_b_open {
                        output.reopen_peer_b = ui.button("Reopen Peer B").clicked();
                    }
                }

                ui.separator();
                if ui.button("Add root").clicked() {
                    self.add_root();
                }

                ui.separator();
                ui.weak(self.document.kind().label());
                ui.label(format!("{} nodes", self.document.len()));
            });
        });
        output
    }

    fn bookmarks_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Bookmarks");
        ui.separator();
        let bookmarks = self.document.bookmarks();
        if bookmarks.is_empty() {
            ui.weak("No bookmarks yet.");
        }
        for id in bookmarks {
            let text = self.document.node_contents(&id).unwrap_or_default();
            let snippet: String = text.lines().next().unwrap_or("").chars().take(24).collect();
            let label = if snippet.is_empty() {
                format!("#{id} (empty)")
            } else {
                format!("#{id} {snippet}")
            };
            if ui.button(label).clicked() {
                self.document.set_active(&id);
                self.selected = Some(id);
            }
        }
    }

    fn inspector_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.separator();
        self.sync_edit_buffer();

        let Some(id) = self.selected else {
            ui.weak("No node selected. Click a node in the tree.");
            return;
        };
        let Some(info) = self.document.node_info(&id) else {
            self.selected = None;
            return;
        };

        ui.label(RichText::new(format!("Node #{id}")).strong());
        if info.parents.is_empty() {
            ui.label("Parents: — (root)");
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.label("Parents:");
                for parent in &info.parents {
                    if ui.button(format!("#{parent}")).clicked() {
                        self.selected = Some(*parent);
                    }
                }
            });
        }
        ui.label(format!(
            "Active: {}   Bookmarked: {}",
            info.active, info.bookmarked
        ));
        ui.label(format!("Length: {} bytes", info.content_len));
        ui.separator();

        let status = self.edit_status();
        ui.horizontal_wrapped(|ui| {
            ui.label("Contents:");
            buffer_status_label(
                ui,
                status,
                "Node changed; staged edit is stale",
                "The node's contents changed while this edit was staged, for example \
                 because a path edit split it. Copy anything you want to keep, then \
                 Reset to reload the node.",
            );
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.edit_buffer)
                .desired_width(f32::INFINITY)
                .desired_rows(6),
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    status == BufferStatus::Staged,
                    egui::Button::new("Apply edit"),
                )
                .clicked()
            {
                self.apply_edit_buffer(id);
            }
            if ui
                .add_enabled(status != BufferStatus::Clean, egui::Button::new("Reset"))
                .on_hover_text("Discard staged typing and reload the node's contents.")
                .clicked()
            {
                self.reset_edit_buffer();
            }
            if copy_button(ui, status, &self.edit_buffer) {
                self.status = format!("Copied the staged text for #{id} to the clipboard");
            }
        });
        ui.separator();

        ui.horizontal_wrapped(|ui| {
            if ui.button("Set active").clicked() {
                self.document.set_active(&id);
            }
            if ui.button("Set inactive").clicked() {
                self.document.set_inactive(&id);
            }
            if ui
                .button(if info.bookmarked {
                    "Unbookmark"
                } else {
                    "Bookmark"
                })
                .clicked()
            {
                self.document.set_bookmarked(&id, !info.bookmarked);
            }
            if ui.button("Add child").clicked() {
                self.add_child(id);
            }
        });

        if self.document.kind() == WeaveKind::DependentLoro {
            ui.horizontal(|ui| {
                ui.add_enabled(false, egui::Button::new("Split here"));
                ui.add_enabled(false, egui::Button::new("Merge with parent"));
            });
            ui.weak("Split and merge are unavailable for DependentLoroWeave documents.");
        } else {
            if info.content_len >= 2 {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.split_index).range(1..=info.content_len - 1),
                    );
                    if ui.button("Split here").clicked() {
                        self.split_selected(id);
                    }
                });
            }
            if !info.parents.is_empty() && ui.button("Merge with parent").clicked() {
                self.merge_selected(id);
            }
        }

        ui.horizontal_wrapped(|ui| {
            if info.children.len() >= 2 && ui.button("Sort children A→Z").clicked() {
                self.document.sort_children(&id);
            }
            if info.children.len() >= 2 && ui.button("Sort children by ID").clicked() {
                self.document.sort_children_by_id(&id);
            }
        });

        if self.document.kind() == WeaveKind::Independent {
            ui.separator();
            ui.label("Move node — parent ids (comma-separated, empty = root):");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.move_buffer).desired_width(140.0));
                if ui.button("Move").clicked() {
                    self.move_selected(id);
                }
            });
        }
        ui.separator();

        if !info.children.is_empty() {
            ui.label("Children:");
            ui.horizontal_wrapped(|ui| {
                for child in &info.children {
                    if ui.button(format!("#{child}")).clicked() {
                        self.selected = Some(*child);
                    }
                }
            });
            ui.separator();
        }
        if ui
            .button(RichText::new("Delete subtree").color(Color32::LIGHT_RED))
            .clicked()
        {
            self.delete_selected(id);
        }
    }

    fn reading_view(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("reading")
            .resizable(true)
            .default_size(170.0)
            .show(ui, |ui| {
                let editable = self.document.kind() == WeaveKind::Independent;
                let status = if editable {
                    self.sync_reading_buffer()
                } else {
                    BufferStatus::Clean
                };
                let editor_id = ui.make_persistent_id("active_path_editor");
                // The shortcut fires only while the editor itself has keyboard focus, so it
                // never takes Enter away from the inspector or the title field. It is
                // consumed whatever the buffer's state, so a stale or unchanged edit
                // reports why nothing was applied instead of silently doing nothing.
                let mut apply = ui.memory(|memory| memory.has_focus(editor_id))
                    && ui.input_mut(|input| input.consume_shortcut(&APPLY_SHORTCUT));
                let mut reset = false;
                let mut restore = false;
                let mut copy = false;
                let path = self.document.active_path();

                // The controls live in the heading row so a long path can never push them
                // out of the panel.
                ui.horizontal(|ui| {
                    ui.heading("Reading view");
                    if !editable {
                        return;
                    }
                    ui.separator();
                    buffer_status_label(
                        ui,
                        status,
                        "Active path changed; staged edit is stale",
                        "The staged text no longer matches the path it was typed on. Restore \
                         path makes that path active again so the edit applies; otherwise \
                         copy the staged text and Reset to reload the current path.",
                    );
                    if status == BufferStatus::Stale {
                        let path_moved = path != self.reading_original_path;
                        let restorable = path_moved
                            && self.document.can_restore_path(&self.reading_original_path);
                        restore = ui
                            .add_enabled(restorable, egui::Button::new("Restore path"))
                            .on_hover_text(
                                "Make the path this edit was typed on active again, so the \
                                 staged edit can be applied.",
                            )
                            .on_disabled_hover_text(if path_moved {
                                "Nodes on that path were removed or moved, so it cannot be \
                                 restored."
                            } else {
                                "That path is still active; its text changed underneath the \
                                 edit instead."
                            })
                            .clicked();
                    }
                    let shortcut = ui.ctx().format_shortcut(&APPLY_SHORTCUT);
                    apply |= ui
                        .add_enabled(
                            status == BufferStatus::Staged,
                            egui::Button::new("Apply staged edit"),
                        )
                        .on_hover_text(format!(
                            "{shortcut} — turns each changed range into a branch-preserving \
                             path patch; replaced text stays on alternate branches."
                        ))
                        .clicked();
                    reset = ui
                        .add_enabled(status != BufferStatus::Clean, egui::Button::new("Reset"))
                        .on_hover_text("Discard staged typing and reload the current active path.")
                        .clicked();
                    copy = copy_button(ui, status, &self.reading_buffer);
                });
                ui.separator();

                if path.is_empty() && !editable {
                    ui.weak("No active node. Double-click a node or use “Set active”.");
                    return;
                }
                let mut crumbs = path
                    .iter()
                    .rev()
                    .map(|id| format!("#{id}"))
                    .collect::<Vec<_>>();
                if let Some(last) = crumbs.last_mut() {
                    last.push_str(" (active)");
                }
                ui.weak(crumbs.join(" → "));

                if editable {
                    // The editor grows with its text, so it scrolls inside the panel.
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.reading_buffer)
                                    .id(editor_id)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(5),
                            );
                        });
                    if apply {
                        self.apply_reading_buffer();
                    } else if reset {
                        self.reset_reading_buffer();
                    } else if restore {
                        self.restore_reading_path();
                    } else if copy {
                        self.status =
                            "Copied the staged active-path text to the clipboard".to_string();
                    }
                } else {
                    let text = path
                        .iter()
                        .rev()
                        .filter_map(|id| self.document.node_contents(id))
                        .collect::<String>();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text).size(15.0)).wrap());
                    });
                }
            });
    }

    fn action_log(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("action_log")
            .resizable(true)
            .default_size(130.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Action log");
                    ui.weak(format!("({} logged)", self.document.action_count()));
                    if ui.button("Clear").clicked() {
                        self.document.clear_actions();
                    }
                });
                ui.separator();
                let actions = self.document.formatted_actions();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if actions.is_empty() {
                        ui.weak("No local actions awaiting synchronization.");
                    }
                    for action in actions {
                        ui.label(RichText::new(action).monospace().small());
                    }
                });
            });
    }

    fn show(
        &mut self,
        ui: &mut egui::Ui,
        peer_name: &str,
        connected: &mut bool,
        primary: bool,
        peer_b_open: bool,
        new_kind: &mut WeaveKind,
    ) -> EditorOutput {
        if self.selected.is_some_and(|id| !self.document.contains(&id)) {
            self.selected = None;
            self.edit_for = None;
        }

        let output = self.toolbar(ui, peer_name, connected, primary, peer_b_open, new_kind);
        egui::Panel::right("inspector")
            .default_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.inspector_view(ui);
                    ui.add_space(8.0);
                    ui.separator();
                    self.bookmarks_view(ui);
                });
            });
        egui::Panel::bottom("status")
            .exact_size(26.0)
            .show(ui, |ui| {
                ui.label(&self.status);
            });
        self.action_log(ui);
        self.reading_view(ui);

        let selected = self.selected;
        let active = self.document.active_set();
        let path: HashSet<u64> = self.document.active_path().into_iter().collect();
        let tree_layout = self.document.tree_layout();
        let nodes = self.document.tree_nodes();

        let response = egui::CentralPanel::default()
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        tree_view::show(ui, &nodes, &tree_layout, selected, &active, &path)
                    })
                    .inner
            })
            .inner;

        if let Some(id) = response.clicked {
            self.selected = Some(id);
        }
        if let Some(id) = response.double_clicked {
            self.document.toggle_active(&id);
            self.selected = Some(id);
        }
        output
    }
}

/// Session-level state shared between the editors.
pub struct DemoApp {
    peer_a: EditorState,
    peer_b: Option<EditorState>,
    connected: bool,
    peer_b_open: bool,
    new_kind: WeaveKind,
}

impl DemoApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let document = seeded_dependent();
        let mut peer_a = EditorState::new(
            document,
            5,
            1,
            "Welcome! Click nodes to select, double-click to make active.".to_string(),
        );
        peer_a.selected = Some(3);
        Self {
            peer_a,
            peer_b: None,
            connected: false,
            peer_b_open: false,
            new_kind: WeaveKind::default(),
        }
    }

    fn collaborative_ids(max_id: Option<u64>) -> (u64, u64) {
        let above = max_id.unwrap_or(0).saturating_add(1);
        let odd = if above % 2 == 1 {
            above
        } else {
            above.saturating_add(1)
        };
        let even = if above.is_multiple_of(2) {
            above
        } else {
            above.saturating_add(1)
        };
        (odd, even)
    }

    fn replace_session(&mut self, mut document: Document, status: String) {
        self.new_kind = document.kind();
        let selected = document.active_tip();
        if document.kind() == WeaveKind::DependentLoro {
            match document.fork_collaborative() {
                Ok(peer_b_document) => {
                    let (next_a, next_b) = Self::collaborative_ids(document.max_id());
                    let mut peer_a = EditorState::new(document, next_a, 2, status.clone());
                    peer_a.selected = selected;
                    let mut peer_b = EditorState::new(
                        peer_b_document,
                        next_b,
                        2,
                        "Peer B ready — collaboration connected.".to_string(),
                    );
                    peer_b.selected = selected;
                    self.peer_a = peer_a;
                    self.peer_b = Some(peer_b);
                    self.connected = true;
                    self.peer_b_open = true;
                }
                Err(error) => {
                    self.peer_a.status = format!("Could not start collaboration: {error}");
                }
            }
        } else {
            let next_id = document.max_id().map_or(0, |id| id.saturating_add(1));
            let mut peer_a = EditorState::new(document, next_id, 1, status);
            peer_a.selected = selected;
            self.peer_a = peer_a;
            self.peer_b = None;
            self.connected = false;
            self.peer_b_open = false;
        }
    }

    /// Typed input in either peer that has not been applied to its document.
    fn unapplied_edits(&self) -> Vec<String> {
        let Some(peer_b) = &self.peer_b else {
            return self.peer_a.unapplied_edits();
        };
        let attribute = |edits: Vec<String>, peer: &'static str| {
            edits
                .into_iter()
                .map(move |edit| format!("{edit} in {peer}"))
        };
        attribute(self.peer_a.unapplied_edits(), "Peer A")
            .chain(attribute(peer_b.unapplied_edits(), "Peer B"))
            .collect()
    }

    /// Asks before an action that would leave unapplied edits behind, returning whether
    /// to proceed. A declined action is reported in the status bar.
    fn confirm_despite_unapplied_edits(&mut self, action: &str, question: &str) -> bool {
        let pending = self.unapplied_edits();
        if pending.is_empty() {
            return true;
        }
        let proceed = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title(format!("{action} with unapplied edits?"))
            .set_description(format!("Unapplied: {}.\n\n{question}", pending.join("; ")))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !proceed {
            self.peer_a.status =
                format!("{action} cancelled; apply or reset the unapplied edits first");
        }
        proceed
    }

    fn new_document(&mut self) {
        if !self.confirm_despite_unapplied_edits(
            "Create",
            "Creating a new document discards them. Discard the edits and continue?",
        ) {
            return;
        }
        let document = Document::empty(self.new_kind);
        self.replace_session(document, format!("New {} document", self.new_kind.label()));
    }

    fn open_document(&mut self) {
        if !self.confirm_despite_unapplied_edits(
            "Open",
            "Opening another document discards them. Discard the edits and choose a file?",
        ) {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Universal Weave demo", &["uweave"])
            .pick_file()
        else {
            return;
        };
        match persistence::load_document(&path) {
            Ok(document) => self.replace_session(document, format!("Opened {}", path.display())),
            Err(error) => self.peer_a.status = format!("Open failed: {error}"),
        }
    }

    fn save_document(&mut self) {
        if !self.confirm_despite_unapplied_edits(
            "Save",
            "The saved file will not include them. Save the document without them?",
        ) {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Universal Weave demo", &["uweave"])
            .set_file_name("document.uweave")
            .save_file()
        else {
            return;
        };
        match persistence::save_document(&path, &self.peer_a.document) {
            Ok(()) => self.peer_a.status = format!("Saved {}", path.display()),
            Err(error) => self.peer_a.status = format!("Save failed: {error}"),
        }
    }

    fn synchronize(&mut self, ctx: &egui::Context) -> Option<SyncOutcome> {
        if !self.connected {
            return None;
        }
        let Some(peer_b) = self.peer_b.as_mut() else {
            self.connected = false;
            return None;
        };

        let a_title_dirty = self.peer_a.title_is_dirty();
        let b_title_dirty = peer_b.title_is_dirty();
        match synchronize_pair(&mut self.peer_a.document, &mut peer_b.document) {
            Ok(outcome) => {
                if outcome.peer_a_changed {
                    self.peer_a.refresh_after_import(a_title_dirty);
                }
                if outcome.peer_b_changed {
                    peer_b.refresh_after_import(b_title_dirty);
                }
                if outcome.peer_a_changed || outcome.peer_b_changed {
                    self.peer_a.status = "Synchronized with Peer B".to_string();
                    peer_b.status = "Synchronized with Peer A".to_string();
                    ctx.request_repaint();
                }
                Some(outcome)
            }
            Err(error) => {
                self.connected = false;
                self.peer_a.status = format!("Synchronization failed; now offline: {error}");
                peer_b.status = format!("Synchronization failed; now offline: {error}");
                None
            }
        }
    }
}

impl eframe::App for DemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.synchronize(&ctx);

        let output = self.peer_a.show(
            ui,
            "Peer A",
            &mut self.connected,
            true,
            self.peer_b_open,
            &mut self.new_kind,
        );

        if output.open {
            self.open_document();
        } else if output.save {
            self.save_document();
        } else if output.create {
            self.new_document();
        }
        if output.reopen_peer_b && self.peer_b.is_some() {
            self.peer_b_open = true;
        }

        if self.peer_b_open
            && let Some(peer_b) = self.peer_b.as_mut()
        {
            let close_requested = ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(PEER_B_VIEWPORT),
                egui::ViewportBuilder::default()
                    .with_title("Universal Weave Demo — Peer B")
                    .with_inner_size([1200.0, 780.0])
                    .with_min_inner_size([760.0, 520.0]),
                |ui, _class| {
                    peer_b.show(
                        ui,
                        "Peer B",
                        &mut self.connected,
                        false,
                        true,
                        &mut self.new_kind,
                    );
                    ui.input(|input| input.viewport().close_requested())
                },
            );
            if close_requested {
                self.peer_b_open = false;
            }
        }

        // Reconcile immediately when either viewport reconnects or edits during this frame.
        self.synchronize(&ctx);
        if self.connected {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{seeded_collaborative, seeded_independent, synchronize_pair};

    #[test]
    fn collaborative_ids_use_disjoint_odd_even_sequences_above_maximum() {
        for max in [Some(0), Some(5), Some(100), None] {
            let (a, b) = DemoApp::collaborative_ids(max);
            assert_eq!(a % 2, 1);
            assert_eq!(b % 2, 0);
            assert_ne!(a, b);
            if let Some(max) = max {
                assert!(a > max);
                assert!(b > max);
            }
            assert_ne!(a.saturating_add(2), b.saturating_add(2));
        }
    }

    #[test]
    fn imported_changes_refresh_clean_buffers_and_preserve_dirty_text() {
        let document = seeded_collaborative();
        let mut remote = document.fork_collaborative().unwrap();
        let mut editor = EditorState::new(document, 5, 2, String::new());
        editor.selected = Some(3);
        editor.sync_edit_buffer();
        assert!(!editor.edit_is_dirty());

        remote.apply_edit(&3, "remote value".to_string());
        synchronize_pair(&mut editor.document, &mut remote).unwrap();
        editor.refresh_after_import(false);
        assert_eq!(editor.edit_buffer, "remote value");
        assert_eq!(editor.edit_status(), BufferStatus::Clean);

        editor.edit_buffer = "unapplied local typing".to_string();
        remote.apply_edit(&3, "new remote value".to_string());
        synchronize_pair(&mut editor.document, &mut remote).unwrap();
        editor.refresh_after_import(false);
        assert_eq!(editor.edit_buffer, "unapplied local typing");
        // The remote change is reported rather than silently overwritten on apply.
        assert_eq!(editor.edit_status(), BufferStatus::Stale);
        editor.apply_edit_buffer(3);
        assert_eq!(
            editor.document.node_contents(&3).as_deref(),
            Some("new remote value")
        );
    }

    #[test]
    fn remote_deletion_clears_the_local_selection() {
        let document = seeded_collaborative();
        let mut remote = document.fork_collaborative().unwrap();
        let mut editor = EditorState::new(document, 5, 2, String::new());
        editor.selected = Some(3);
        editor.sync_edit_buffer();

        assert_eq!(remote.remove(&3), Some(1));
        synchronize_pair(&mut editor.document, &mut remote).unwrap();
        editor.refresh_after_import(false);
        assert_eq!(editor.selected, None);
        assert_eq!(editor.edit_for, None);
    }

    #[test]
    fn inspector_edit_goes_stale_when_a_path_edit_splits_its_node() {
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        editor.selected = Some(0);
        editor.sync_edit_buffer();
        assert_eq!(editor.edit_status(), BufferStatus::Clean);
        editor.edit_buffer.push_str(" [typing]");
        assert_eq!(editor.edit_status(), BufferStatus::Staged);

        editor.reading_buffer = editor.reading_buffer.replacen("Tuesday", "Friday", 1);
        editor.apply_reading_buffer();
        assert_eq!(editor.selected, Some(0));
        assert_eq!(editor.edit_status(), BufferStatus::Stale);
        assert!(editor.edit_buffer.ends_with(" [typing]"));

        // Applying is refused, so the shortened node is not overwritten with its old text.
        let contents = editor.document.node_contents(&0).unwrap();
        assert!(contents.ends_with("on a "));
        let path_text = editor.document.active_path_text().1;
        editor.apply_edit_buffer(0);
        assert!(editor.status.contains("changed while this edit was staged"));
        assert_eq!(editor.document.node_contents(&0).unwrap(), contents);
        assert_eq!(editor.document.active_path_text().1, path_text);

        editor.reset_edit_buffer();
        assert_eq!(editor.edit_status(), BufferStatus::Clean);
        assert_eq!(editor.edit_buffer, contents);
    }

    #[test]
    fn clean_inspector_buffer_follows_its_node_and_apply_refreshes_the_snapshot() {
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        editor.selected = Some(3);
        editor.sync_edit_buffer();
        assert!(
            editor
                .document
                .apply_edit(&3, "changed elsewhere".to_string())
        );
        editor.sync_edit_buffer();
        assert_eq!(editor.edit_buffer, "changed elsewhere");
        assert_eq!(editor.edit_status(), BufferStatus::Clean);

        editor.edit_buffer.push('!');
        assert_eq!(editor.edit_status(), BufferStatus::Staged);
        editor.apply_edit_buffer(3);
        assert_eq!(
            editor.document.node_contents(&3).as_deref(),
            Some("changed elsewhere!")
        );
        assert_eq!(editor.edit_status(), BufferStatus::Clean);
        assert_eq!(editor.status, "Edited contents of #3");
    }

    #[test]
    fn appending_after_add_child_fills_the_new_node() {
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        editor.add_child(3);
        assert_eq!(editor.selected, Some(5));
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Clean);
        editor.reading_buffer.push_str("A new sentence.");
        editor.apply_reading_buffer();
        assert_eq!(
            editor.document.node_contents(&5).as_deref(),
            Some("A new sentence.")
        );
        assert_eq!(
            editor.status,
            "Applied 1 change to the active path; replaced text remains on alternate branches"
        );
        assert_eq!(editor.selected, Some(5));
        assert_eq!(editor.next_id, 6);
    }

    #[test]
    fn clean_reading_buffer_refreshes_when_the_active_path_changes() {
        let document = seeded_independent();
        let mut editor = EditorState::new(document, 5, 1, String::new());
        let original = editor.reading_buffer.clone();

        assert!(editor.document.set_active(&4));
        editor.sync_reading_buffer();

        assert_ne!(editor.reading_buffer, original);
        assert_eq!(editor.reading_buffer, editor.document.active_path_text().1);
        assert!(!editor.reading_is_dirty());
    }

    #[test]
    fn dirty_reading_buffer_is_preserved_and_flagged_stale_until_reset() {
        let document = seeded_independent();
        let mut editor = EditorState::new(document, 5, 1, String::new());
        editor.reading_buffer.push_str("local typing");
        let staged = editor.reading_buffer.clone();
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Staged);

        assert!(editor.document.set_active(&4));
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Stale);
        assert_eq!(editor.reading_buffer, staged);

        // The document still rejects the stale edit if Apply is forced.
        editor.apply_reading_buffer();
        assert!(editor.status.contains("changed while this edit was staged"));
        assert_eq!(editor.reading_buffer, staged);

        editor.reset_reading_buffer();
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Clean);
        assert_eq!(editor.reading_buffer, editor.document.active_path_text().1);
        assert!(!editor.reading_is_dirty());
    }

    #[test]
    fn restoring_the_path_makes_a_stale_edit_applicable() {
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        editor.reading_buffer.push_str(" typed");
        let original = editor.reading_original_path.clone();
        assert!(editor.document.set_active(&4));
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Stale);
        assert!(editor.document.can_restore_path(&original));

        editor.restore_reading_path();
        assert_eq!(
            editor.status,
            "Restored the active path; the staged edit can be applied"
        );
        assert_eq!(editor.document.active_path(), original);
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Staged);
        editor.apply_reading_buffer();
        assert!(
            editor.status.starts_with("Applied 1 change"),
            "{}",
            editor.status
        );
        assert!(editor.document.active_path_text().1.ends_with(" typed"));

        // A path whose text changed underneath the edit stays stale after restoring.
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        editor.reading_buffer.push_str(" typed");
        assert!(editor.document.apply_edit(&1, "rewritten".to_string()));
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Stale);
        editor.restore_reading_path();
        assert!(editor.status.contains("text changed"), "{}", editor.status);
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Stale);

        // A path with a removed node cannot be restored, and the failure is reported.
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        editor.reading_buffer.push_str(" typed");
        assert_eq!(editor.document.remove(&3), Some(1));
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Stale);
        assert!(
            !editor
                .document
                .can_restore_path(&editor.reading_original_path)
        );
        editor.restore_reading_path();
        assert!(
            editor.status.contains("can no longer be restored"),
            "{}",
            editor.status
        );
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Stale);
    }

    #[test]
    fn applying_reports_reused_branches() {
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        editor.reading_buffer = editor.reading_buffer.replacen("Tuesday", "Friday", 1);
        editor.apply_reading_buffer();
        let nodes = editor.document.len();
        editor.reading_buffer = editor.reading_buffer.replacen("Friday", "Tuesday", 1);
        editor.apply_reading_buffer();
        assert_eq!(
            editor.status,
            "Applied 1 change to the active path, switching to 1 existing branch; \
             replaced text remains on alternate branches"
        );
        assert_eq!(editor.document.len(), nodes);
        assert!(editor.document.active_path_text().1.contains("Tuesday"));
    }

    #[test]
    fn reading_buffer_apply_reports_selects_and_noop_refreshes_the_snapshot() {
        let document = seeded_independent();
        let mut editor = EditorState::new(document, 5, 1, String::new());
        editor.reading_buffer.push_str(" new ending");
        editor.apply_reading_buffer();
        assert_eq!(editor.reading_buffer, editor.document.active_path_text().1);
        assert_eq!(editor.reading_buffer, editor.reading_original_text);
        assert!(!editor.reading_is_dirty());
        assert_eq!(editor.sync_reading_buffer(), BufferStatus::Clean);
        assert!(
            editor
                .status
                .starts_with("Applied 1 change to the active path, creating 1 node;"),
            "{}",
            editor.status
        );
        let selected = editor.selected.expect("the inserted node is selected");
        assert_eq!(
            editor.document.node_contents(&selected).as_deref(),
            Some(" new ending")
        );

        let actions = editor.document.action_count();
        editor.apply_reading_buffer();
        assert_eq!(editor.status, "Active-path edit is unchanged");
        assert_eq!(editor.document.action_count(), actions);
    }

    #[test]
    fn applying_keeps_a_dirty_inspector_selection() {
        let document = seeded_independent();
        let mut editor = EditorState::new(document, 5, 1, String::new());
        editor.selected = Some(1);
        editor.sync_edit_buffer();
        editor.edit_buffer.push_str(" unapplied");
        editor.reading_buffer.push_str(" new ending");
        editor.apply_reading_buffer();
        assert_eq!(editor.selected, Some(1));
        assert!(editor.edit_buffer.ends_with(" unapplied"));
    }

    #[test]
    fn unapplied_edits_cover_staged_typing_and_inspector_edits() {
        let mut editor = EditorState::new(seeded_independent(), 5, 1, String::new());
        assert!(editor.unapplied_edits().is_empty());
        editor.reading_buffer.push_str(" typing");
        assert_eq!(
            editor.unapplied_edits(),
            vec!["a staged active-path edit".to_string()]
        );
        editor.selected = Some(3);
        editor.sync_edit_buffer();
        editor.edit_buffer.push_str(" more");
        assert_eq!(
            editor.unapplied_edits(),
            vec![
                "a staged active-path edit".to_string(),
                "an unapplied edit to node #3".to_string()
            ]
        );
        editor.reset_reading_buffer();
        assert_eq!(
            editor.unapplied_edits(),
            vec!["an unapplied edit to node #3".to_string()]
        );

        let mut app = DemoApp {
            peer_a: editor,
            peer_b: None,
            connected: false,
            peer_b_open: false,
            new_kind: WeaveKind::Independent,
        };
        assert_eq!(
            app.unapplied_edits(),
            vec!["an unapplied edit to node #3".to_string()]
        );

        // With two collaborative peers, edits are attributed to their peer.
        let document = seeded_collaborative();
        let mut peer_b =
            EditorState::new(document.fork_collaborative().unwrap(), 6, 2, String::new());
        peer_b.selected = Some(3);
        peer_b.sync_edit_buffer();
        peer_b.edit_buffer.push_str(" from B");
        app.peer_a = EditorState::new(document, 5, 2, String::new());
        app.peer_b = Some(peer_b);
        assert_eq!(
            app.unapplied_edits(),
            vec!["an unapplied edit to node #3 in Peer B".to_string()]
        );
    }
}
