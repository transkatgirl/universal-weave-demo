//! The main application state and UI.

use std::collections::HashSet;

use eframe::egui::{self, Color32, RichText};

use crate::document::{Document, WeaveKind, seeded_dependent};
use crate::{persistence, tree_view};

pub struct DemoApp {
    document: Document,
    next_id: u64,
    selected: Option<u64>,
    /// Contents of the currently selected node, edited locally until applied.
    edit_buffer: String,
    /// Which node `edit_buffer` was loaded from (used to detect selection changes).
    edit_for: Option<u64>,
    split_index: usize,
    title_buffer: String,
    /// The weave implementation used when creating a new document.
    new_kind: WeaveKind,
    /// Raw input for the "move node" field (independent documents only).
    move_buffer: String,
    status: String,
}

impl DemoApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let document = seeded_dependent();
        let title_buffer = document.metadata().clone();

        Self {
            document,
            next_id: 5,
            selected: Some(3),
            edit_buffer: String::new(),
            edit_for: None,
            split_index: 0,
            title_buffer,
            new_kind: WeaveKind::default(),
            move_buffer: String::new(),
            status: "Welcome! Click nodes to select, double-click to make active.".to_string(),
        }
    }

    /// Reloads the edit buffer when the selection changes or contents were
    /// replaced by an operation (split/merge).
    fn sync_edit_buffer(&mut self) {
        if self.edit_for != self.selected {
            self.edit_buffer = self
                .selected
                .and_then(|id| self.document.node_contents(&id))
                .unwrap_or_default();
            self.edit_for = self.selected;
            self.split_index = self.edit_buffer.len() / 2;
            self.move_buffer.clear();
        }
    }

    fn add_root(&mut self) {
        let id = self.next_id;
        if self.document.add_root(id) {
            self.next_id += 1;
            self.selected = Some(id);
            self.status = format!("Added root node #{id}");
        } else {
            self.status = "Failed to add root node".to_string();
        }
    }

    fn add_child(&mut self, parent: u64) {
        let id = self.next_id;
        if self.document.add_child(&parent, id) {
            self.next_id += 1;
            self.selected = Some(id);
            self.status = format!("Added child node #{id} under #{parent}");
        } else {
            self.status = format!("Failed to add child under #{parent}");
        }
    }

    fn split_selected(&mut self, id: u64) {
        let at = self.split_index;
        let new_id = self.next_id;

        if self.document.split(&id, at, new_id) {
            self.next_id += 1;
            self.edit_for = None; // contents changed; force edit buffer reload
            self.status = format!("Split node #{id} at byte {at}; tail became #{new_id}");
        } else {
            self.status = format!("Could not split #{id} at byte {at} (not a char boundary?)");
        }
    }

    fn merge_selected(&mut self, id: u64) {
        match self.document.merge_with_parent(&id) {
            Some(parent) => {
                self.selected = Some(parent);
                self.edit_for = None;
                self.status = format!("Merged node #{id} into its parent #{parent}");
            }
            None => {
                self.status = format!(
                    "Could not merge #{id} (root, multiple parents, or parent has other children)"
                );
            }
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
                    self.status = if parents.is_empty() {
                        format!("Moved node #{id} to the roots")
                    } else {
                        format!("Moved node #{id} under {parents:?}")
                    };
                    self.move_buffer.clear();
                }
                Err(e) => self.status = e,
            },
            Err(_) => {
                self.status = format!("Invalid parent list: \"{}\"", self.move_buffer);
            }
        }
    }

    fn find_duplicates(&mut self, id: u64) {
        let duplicates = self.document.find_duplicates(&id);

        if duplicates.is_empty() {
            self.status = format!("Node #{id} has no duplicate siblings");
        } else {
            let list: Vec<String> = duplicates.iter().map(|d| format!("#{d}")).collect();
            self.status = format!("Node #{id} duplicates: {}", list.join(", "));
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

    fn new_document(&mut self) {
        self.document = Document::empty(self.new_kind);
        self.next_id = 1;
        self.selected = Some(0);
        self.edit_for = None;
        self.title_buffer = self.document.metadata().clone();
        self.status = format!("New {} document", self.new_kind.label());
    }

    fn open_document(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Universal Weave demo", &["uweave"])
            .pick_file()
        else {
            return;
        };

        match persistence::load_document(&path) {
            Ok(document) => {
                self.next_id = document.max_id().map_or(0, |max| max + 1);
                self.title_buffer = document.metadata().clone();
                self.new_kind = document.kind();
                self.document = document;
                self.selected = self.document.active_tip();
                self.edit_for = None;
                self.status = format!("Opened {}", path.display());
            }
            Err(e) => {
                self.status = format!("Open failed: {e}");
            }
        }
    }

    fn save_document(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Universal Weave demo", &["uweave"])
            .set_file_name("document.uweave")
            .save_file()
        else {
            return;
        };

        match persistence::save_document(&path, &self.document) {
            Ok(()) => self.status = format!("Saved {}", path.display()),
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("Universal Weave");
                ui.separator();

                ui.label("Title:");
                let response = ui.text_edit_singleline(&mut self.title_buffer);
                if response.lost_focus() && self.title_buffer != *self.document.metadata() {
                    self.document.set_metadata(self.title_buffer.clone());
                }
                ui.separator();

                if ui.button("Open…").clicked() {
                    self.open_document();
                }
                if ui.button("Save…").clicked() {
                    self.save_document();
                }
                ui.separator();

                ui.label("New:");
                egui::ComboBox::from_id_salt("new_weave_kind")
                    .selected_text(self.new_kind.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.new_kind,
                            WeaveKind::Dependent,
                            WeaveKind::Dependent.label(),
                        );
                        ui.selectable_value(
                            &mut self.new_kind,
                            WeaveKind::Independent,
                            WeaveKind::Independent.label(),
                        );
                    });
                if ui.button("Create").clicked() {
                    self.new_document();
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
    }

    fn bookmarks_view(&mut self, ui: &mut egui::Ui) {
                ui.heading("Bookmarks");
                ui.separator();

                if self.document.bookmarks().is_empty() {
                    ui.weak("No bookmarks yet.");
                }

                for id in self.document.bookmarks() {
                    let label = self
                        .document
                        .node_contents(&id)
                        .map(|text| {
                            let snippet: String =
                                text.lines().next().unwrap_or("").chars().take(24).collect();
                            if snippet.is_empty() {
                                format!("#{id} (empty)")
                            } else {
                                format!("#{id} {snippet}")
                            }
                        })
                        .unwrap_or_else(|| format!("#{id}"));

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

                ui.label("Contents:");
                ui.add(
                    egui::TextEdit::multiline(&mut self.edit_buffer)
                        .desired_width(f32::INFINITY)
                        .desired_rows(6),
                );
                if ui.button("Apply edit").clicked() {
                    let text = std::mem::take(&mut self.edit_buffer);
                    self.document.apply_edit(&id, text.clone());
                    self.edit_buffer = text;
                    self.status = format!("Edited contents of #{id}");
                }
                ui.separator();

                ui.horizontal_wrapped(|ui| {
                    if ui.button("Set active").clicked() {
                        self.document.set_active(&id);
                    }
                    if ui.button("Set inactive").clicked() {
                        self.document.set_inactive(&id);
                    }
                    let bookmark_label = if info.bookmarked { "Unbookmark" } else { "Bookmark" };
                    if ui.button(bookmark_label).clicked() {
                        self.document.set_bookmarked(&id, !info.bookmarked);
                    }
                    if ui.button("Add child").clicked() {
                        self.add_child(id);
                    }
                });

                if info.content_len >= 2 {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.split_index)
                                .range(1..=info.content_len - 1),
                        );
                        if ui.button("Split here").clicked() {
                            self.split_selected(id);
                        }
                    });
                }

                ui.horizontal_wrapped(|ui| {
                    if !info.parents.is_empty() && ui.button("Merge with parent").clicked() {
                        self.merge_selected(id);
                    }
                    if info.children.len() >= 2 && ui.button("Sort children A→Z").clicked() {
                        self.document.sort_children(&id);
                    }
                    if ui.button("Find duplicates").clicked() {
                        self.find_duplicates(id);
                    }
                });

                if self.document.kind() == WeaveKind::Independent {
                    ui.separator();
                    ui.label("Move node — new parent ids (comma-separated, empty = root):");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.move_buffer)
                                .desired_width(140.0),
                        );
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

    fn inspector_panel(&mut self, ui: &mut egui::Ui) {
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
    }

    fn reading_view(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("reading")
            .resizable(true)
            .default_size(170.0)
            .show(ui, |ui| {
                ui.heading("Reading view");
                ui.separator();

                let path = self.document.active_path();

                if path.is_empty() {
                    ui.weak("No active node. Double-click a node or use “Set active”.");
                    return;
                }

                let mut crumbs: Vec<String> =
                    path.iter().rev().map(|id| format!("#{id}")).collect();
                if let Some(last) = crumbs.last_mut() {
                    last.push_str(" (active)");
                }
                ui.weak(crumbs.join(" → "));

                let mut text = String::new();
                for id in path.iter().rev() {
                    if let Some(contents) = self.document.node_contents(id) {
                        text.push_str(&contents);
                    }
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(text).size(15.0)).wrap());
                });
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
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if actions.is_empty() {
                            ui.weak("No actions yet. Actions performed through the UI are logged by the LoggedWeave wrapper.");
                        }
                        for action in &actions {
                            ui.label(RichText::new(action).monospace().small());
                        }
                    });
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status")
            .exact_size(26.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&self.status);
                });
            });
    }
}

impl eframe::App for DemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Forget selections pointing at nodes that no longer exist.
        if self.selected.is_some_and(|id| !self.document.contains(&id)) {
            self.selected = None;
        }

        self.toolbar(ui);
        self.inspector_panel(ui);
        self.status_bar(ui);
        self.action_log(ui);
        self.reading_view(ui);

        egui::CentralPanel::default().show(ui, |ui| {
            let active = self.document.active_set();
            let path: HashSet<u64> = self.document.active_path().into_iter().collect();
            let nodes = self.document.tree_nodes();

            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let response = tree_view::show(ui, &nodes, self.selected, &active, &path);

                    if let Some(id) = response.clicked {
                        self.selected = Some(id);
                    }
                    if let Some(id) = response.double_clicked {
                        self.document.toggle_active(&id);
                        self.selected = Some(id);
                    }
                });
        });
    }
}
