//! Application state, device-input handling, and embedded UI.
//!
//! The device exposes a joystick (four directions + press) and three buttons.
//! All interaction is mode-based: the joystick selects nodes, button 1 opens
//! an action menu, button 2 cycles the info panel, and button 3 exits. While
//! the reading panel is shown it expands to half the screen and buttons 1/3
//! scroll it instead. Text is entered through a joystick-driven on-screen
//! keyboard.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use hashbrown::HashSet;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::{Key, Keyboard, KeyboardPage};

use super::document::{Document, WeaveKind};
use super::input::Input;
use super::tree_view::{self, GraphViewport, NavigationDirection, TreeLayout, TreeNode, TreeView};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AppEvent {
    #[default]
    None,
    Save {
        exit_after: bool,
    },
    Close,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum CompactTab {
    #[default]
    Inspector,
    Bookmarks,
    Reading,
}

impl CompactTab {
    const ALL: [Self; 3] = [Self::Inspector, Self::Bookmarks, Self::Reading];

    const fn index(self) -> usize {
        match self {
            Self::Inspector => 0,
            Self::Bookmarks => 1,
            Self::Reading => 2,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Inspector => "Inspector",
            Self::Bookmarks => "Bookmarks",
            Self::Reading => "Reading",
        }
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputPurpose {
    Title,
    EditNode(u64),
    SplitNode(u64),
    MoveNode(u64),
}

impl InputPurpose {
    const fn title(self) -> &'static str {
        match self {
            Self::Title => " Edit document title ",
            Self::EditNode(_) => " Edit node contents ",
            Self::SplitNode(_) => " Split at byte offset ",
            Self::MoveNode(_) => " Move to parent IDs ",
        }
    }

    const fn multiline(self) -> bool {
        matches!(self, Self::EditNode(_))
    }

    const fn initial_page(self) -> KeyboardPage {
        match self {
            Self::SplitNode(_) | Self::MoveNode(_) => KeyboardPage::Symbols,
            _ => KeyboardPage::Lower,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextBuffer {
    text: String,
    cursor: usize,
}

impl TextBuffer {
    fn new(text: String) -> Self {
        let cursor = text.len();
        Self { text, cursor }
    }

    fn insert(&mut self, value: char) {
        self.text.insert(self.cursor, value);
        self.cursor += value.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some(previous) = self.previous_boundary() {
            self.text.drain(previous..self.cursor);
            self.cursor = previous;
        }
    }

    fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.text.drain(self.cursor..next);
        }
    }

    fn left(&mut self) {
        if let Some(previous) = self.previous_boundary() {
            self.cursor = previous;
        }
    }

    fn right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    fn cursor_line_column(&self) -> (usize, usize) {
        let line = self.text[..self.cursor]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let start = self.line_start();
        let column = UnicodeWidthStr::width(&self.text[start..self.cursor]);
        (line, column)
    }

    fn previous_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|value| self.cursor + value.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyboardDialog {
    purpose: InputPurpose,
    buffer: TextBuffer,
    keyboard: Keyboard,
    keyboard_visible: bool,
}

impl KeyboardDialog {
    fn new(purpose: InputPurpose, value: String) -> Self {
        let mut keyboard = Keyboard::new();
        keyboard.page = purpose.initial_page();
        Self {
            purpose,
            buffer: TextBuffer::new(value),
            keyboard,
            keyboard_visible: true,
        }
    }

    fn move_cursor(&mut self, direction: NavigationDirection) {
        match direction {
            NavigationDirection::Up => self.keyboard.move_up(),
            NavigationDirection::Down => self.keyboard.move_down(),
            NavigationDirection::Left => self.keyboard.move_left(),
            NavigationDirection::Right => self.keyboard.move_right(),
        }
    }

    fn press(&mut self) {
        let key = self.keyboard.selected();
        match key {
            Key::Character(value) => self.buffer.insert(value),
            Key::Space => self.buffer.insert(' '),
            Key::Backspace => self.buffer.backspace(),
            Key::Delete => self.buffer.delete(),
            Key::Enter if self.purpose.multiline() => self.buffer.insert('\n'),
            Key::Case | Key::Page => self.keyboard.activate_meta(key),
            Key::Enter => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    AddChild,
    AddRoot,
    EditContents,
    ToggleActive,
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    SplitNode,
    MergeNode,
    MoveNode,
    SortByContent,
    SortById,
    DeleteNode,
    EditTitle,
    PanZoom,
    #[cfg(test)]
    NewDocument,
    Save,
    ClearLog,
    Help,
    Exit,
}

impl MenuItem {
    const fn label(self) -> &'static str {
        match self {
            Self::AddChild => "Add child node",
            Self::AddRoot => "Add root node",
            Self::EditContents => "Edit node contents",
            Self::ToggleActive => "Toggle active",
            Self::ToggleBookmark => "Toggle bookmark",
            Self::NextBookmark => "Next bookmark",
            Self::PrevBookmark => "Previous bookmark",
            Self::SplitNode => "Split node",
            Self::MergeNode => "Merge into parent",
            Self::MoveNode => "Move node (DAG)",
            Self::SortByContent => "Sort children by content",
            Self::SortById => "Sort children by ID",
            Self::DeleteNode => "Delete node",
            Self::EditTitle => "Edit document title",
            Self::PanZoom => "Pan/zoom view",
            #[cfg(test)]
            Self::NewDocument => "New document",
            Self::Save => "Save",
            Self::ClearLog => "Reset action counters",
            Self::Help => "Help",
            Self::Exit => "Exit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitChoice {
    Save,
    Discard,
    Cancel,
}

impl ExitChoice {
    const ALL: [Self; 3] = [Self::Save, Self::Discard, Self::Cancel];

    const fn index(self) -> usize {
        match self {
            Self::Save => 0,
            Self::Discard => 1,
            Self::Cancel => 2,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Save => "Save & exit",
            Self::Discard => "Discard & exit",
            Self::Cancel => "Cancel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewDocumentChoice {
    Save,
    Discard,
    Cancel,
}

impl NewDocumentChoice {
    const ALL: [Self; 3] = [Self::Save, Self::Discard, Self::Cancel];

    const fn index(self) -> usize {
        match self {
            Self::Save => 0,
            Self::Discard => 1,
            Self::Cancel => 2,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Save => "Save & create new",
            Self::Discard => "Discard & create new",
            Self::Cancel => "Cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Dialog {
    Help(u16),
    Menu(usize),
    NewDocument { kind: WeaveKind, startup: bool },
    ConfirmNewDocument(NewDocumentChoice),
    ConfirmExit(ExitChoice),
    Keyboard(KeyboardDialog),
}

pub struct WeaveApp {
    document: Document,
    next_id: u64,
    selected: Option<u64>,
    status: String,
    nodes: Vec<TreeNode>,
    layout: TreeLayout,
    viewport: GraphViewport,
    dialog: Option<Dialog>,
    compact_tab: CompactTab,
    reading_scroll: u16,
    file_name: String,
    view_mode: bool,
    dirty: bool,
    new_after_save: bool,
    event: AppEvent,
}

impl WeaveApp {
    /// Start from a document loaded from `path`.
    pub fn with_document(document: Document, file_name: String) -> Self {
        let mut app = Self::empty_shell(file_name);
        app.install_document(document, false);
        app.status = "Welcome! Button 1 opens the menu.".to_owned();
        app
    }

    /// Start with a document-kind chooser; the created document will be saved
    /// to `path`.
    pub fn with_new_document(file_name: String) -> Self {
        let mut app = Self::empty_shell(file_name);
        app.install_document(Document::empty(WeaveKind::Dependent), false);
        app.dialog = Some(Dialog::NewDocument {
            kind: WeaveKind::Dependent,
            startup: true,
        });
        app.status = "File not found; choose a document kind to create it.".to_owned();
        app
    }

    fn empty_shell(file_name: String) -> Self {
        Self {
            document: Document::empty(WeaveKind::Dependent),
            next_id: 1,
            selected: None,
            status: String::new(),
            nodes: Vec::new(),
            layout: TreeLayout::default(),
            viewport: GraphViewport::default(),
            dialog: None,
            compact_tab: CompactTab::default(),
            reading_scroll: 0,
            file_name,
            view_mode: false,
            dirty: false,
            new_after_save: false,
            event: AppEvent::None,
        }
    }

    fn install_document(&mut self, mut document: Document, dirty: bool) {
        self.next_id = document.max_id().map_or(0, |id| id.saturating_add(1));
        self.selected = document.active_tip();
        self.document = document;
        self.dirty = dirty;
        self.new_after_save = false;
        self.view_mode = false;
        self.refresh_graph(true);
    }

    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub const fn document(&self) -> &Document {
        &self.document
    }

    fn refresh_graph(&mut self, fit: bool) {
        self.layout = self.document.tree_layout();
        self.nodes = self.document.tree_nodes();
        if fit {
            self.viewport.fit(&self.layout);
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Ask to leave the application, prompting about unsaved changes.
    pub fn request_exit(&mut self) {
        if self.dirty {
            self.dialog = Some(Dialog::ConfirmExit(ExitChoice::Save));
        } else {
            self.event = AppEvent::Close;
        }
    }

    /// Whether a held-down `input` should keep firing. Joystick directions
    /// always repeat; the scroll buttons repeat only while the reading panel
    /// owns them.
    pub fn accepts_repeat(&self, input: Input) -> bool {
        match input {
            Input::Up | Input::Down | Input::Left | Input::Right => true,
            Input::Button1 | Input::Button3 => {
                self.dialog.is_none() && !self.view_mode && self.compact_tab == CompactTab::Reading
            }
            Input::Press | Input::Button2 => false,
        }
    }

    pub fn handle_input(&mut self, input: Input) -> AppEvent {
        if self.dialog.is_some() {
            self.handle_dialog_input(input);
        } else if self.view_mode {
            self.handle_view_input(input);
        } else {
            let reading = self.compact_tab == CompactTab::Reading;
            match input {
                Input::Left => self.select_direction(NavigationDirection::Left),
                Input::Down => self.select_direction(NavigationDirection::Down),
                Input::Up => self.select_direction(NavigationDirection::Up),
                Input::Right => self.select_direction(NavigationDirection::Right),
                Input::Press => self.toggle_active(),
                Input::Button1 if reading => {
                    self.reading_scroll = self.reading_scroll.saturating_sub(1);
                }
                Input::Button1 => self.dialog = Some(Dialog::Menu(0)),
                Input::Button2 => {
                    self.compact_tab = self.compact_tab.next();
                    self.reading_scroll = 0;
                }
                Input::Button3 if reading => {
                    self.reading_scroll = self.reading_scroll.saturating_add(1);
                }
                Input::Button3 => self.request_exit(),
            }
        }
        mem::take(&mut self.event)
    }

    fn handle_view_input(&mut self, input: Input) {
        match input {
            Input::Left => self.viewport.pan(&self.layout, -0.1, 0.0),
            Input::Right => self.viewport.pan(&self.layout, 0.1, 0.0),
            Input::Up => self.viewport.pan(&self.layout, 0.0, 0.1),
            Input::Down => self.viewport.pan(&self.layout, 0.0, -0.1),
            Input::Press => self.viewport.fit(&self.layout),
            Input::Button1 => self.viewport.zoom_by(1.25),
            Input::Button2 => self.viewport.zoom_by(0.8),
            Input::Button3 => self.view_mode = false,
        }
    }

    fn handle_dialog_input(&mut self, input: Input) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        match dialog {
            Dialog::Help(scroll) => match input {
                Input::Up => self.dialog = Some(Dialog::Help(scroll.saturating_sub(1))),
                Input::Down => self.dialog = Some(Dialog::Help(scroll.saturating_add(1))),
                _ => {}
            },
            Dialog::Menu(index) => self.handle_menu_input(input, index),
            Dialog::NewDocument { mut kind, startup } => match input {
                Input::Left | Input::Right => {
                    kind = match kind {
                        WeaveKind::Dependent => WeaveKind::Independent,
                        WeaveKind::Independent => WeaveKind::Dependent,
                    };
                    self.dialog = Some(Dialog::NewDocument { kind, startup });
                }
                Input::Press => {
                    self.install_document(Document::empty(kind), true);
                    self.status = format!("Created a new {} document", kind.label());
                }
                Input::Button3 => {
                    if startup {
                        self.event = AppEvent::Close;
                    }
                }
                _ => self.dialog = Some(Dialog::NewDocument { kind, startup }),
            },
            Dialog::ConfirmNewDocument(choice) => match input {
                Input::Up | Input::Left => {
                    let index = choice.index().checked_sub(1).unwrap_or(2);
                    self.dialog = Some(Dialog::ConfirmNewDocument(NewDocumentChoice::ALL[index]));
                }
                Input::Down | Input::Right => {
                    let index = (choice.index() + 1) % NewDocumentChoice::ALL.len();
                    self.dialog = Some(Dialog::ConfirmNewDocument(NewDocumentChoice::ALL[index]));
                }
                Input::Press => match choice {
                    NewDocumentChoice::Save => {
                        self.new_after_save = true;
                        self.request_save(false);
                    }
                    NewDocumentChoice::Discard => self.open_new_document_chooser(),
                    NewDocumentChoice::Cancel => {}
                },
                Input::Button3 => {}
                _ => self.dialog = Some(Dialog::ConfirmNewDocument(choice)),
            },
            Dialog::ConfirmExit(choice) => match input {
                Input::Up | Input::Left => {
                    let index = choice.index().checked_sub(1).unwrap_or(2);
                    self.dialog = Some(Dialog::ConfirmExit(ExitChoice::ALL[index]));
                }
                Input::Down | Input::Right => {
                    let index = (choice.index() + 1) % ExitChoice::ALL.len();
                    self.dialog = Some(Dialog::ConfirmExit(ExitChoice::ALL[index]));
                }
                Input::Press => match choice {
                    ExitChoice::Save => {
                        self.request_save(true);
                    }
                    ExitChoice::Discard => self.event = AppEvent::Close,
                    ExitChoice::Cancel => {}
                },
                Input::Button3 => {}
                _ => self.dialog = Some(Dialog::ConfirmExit(choice)),
            },
            Dialog::Keyboard(mut entry) => {
                match input {
                    Input::Button1 => return,
                    Input::Button2 => {
                        entry.keyboard_visible = !entry.keyboard_visible;
                    }
                    Input::Button3 => {
                        self.submit_input(entry.purpose, entry.buffer.text);
                        return;
                    }
                    _ if !entry.keyboard_visible => match input {
                        Input::Left => entry.buffer.left(),
                        Input::Right => entry.buffer.right(),
                        Input::Press => entry.keyboard_visible = true,
                        _ => {}
                    },
                    Input::Up => entry.move_cursor(NavigationDirection::Up),
                    Input::Down => entry.move_cursor(NavigationDirection::Down),
                    Input::Left => entry.move_cursor(NavigationDirection::Left),
                    Input::Right => entry.move_cursor(NavigationDirection::Right),
                    Input::Press => entry.press(),
                }
                self.dialog = Some(Dialog::Keyboard(entry));
            }
        }
    }

    fn handle_menu_input(&mut self, input: Input, index: usize) {
        let items = self.menu_items();
        match input {
            Input::Up => {
                let index = index.checked_sub(1).unwrap_or(items.len() - 1);
                self.dialog = Some(Dialog::Menu(index));
            }
            Input::Down => self.dialog = Some(Dialog::Menu((index + 1) % items.len())),
            Input::Left => self.dialog = Some(Dialog::Menu(index.saturating_sub(5))),
            Input::Right => self.dialog = Some(Dialog::Menu((index + 5).min(items.len() - 1))),
            Input::Press => self.execute_menu_item(items[index]),
            Input::Button1 | Input::Button3 => {}
            Input::Button2 => self.dialog = Some(Dialog::Menu(index)),
        }
    }

    fn menu_items(&self) -> Vec<MenuItem> {
        let mut items = vec![
            MenuItem::AddChild,
            MenuItem::AddRoot,
            MenuItem::EditContents,
            MenuItem::ToggleActive,
            MenuItem::ToggleBookmark,
            MenuItem::NextBookmark,
            MenuItem::PrevBookmark,
            MenuItem::SplitNode,
            MenuItem::MergeNode,
        ];
        if self.document.kind() == WeaveKind::Independent {
            items.push(MenuItem::MoveNode);
        }
        items.extend([
            MenuItem::SortByContent,
            MenuItem::SortById,
            MenuItem::DeleteNode,
            MenuItem::EditTitle,
            MenuItem::PanZoom,
            MenuItem::Save,
            MenuItem::ClearLog,
            MenuItem::Help,
            MenuItem::Exit,
        ]);
        items
    }

    fn execute_menu_item(&mut self, item: MenuItem) {
        match item {
            MenuItem::AddChild => self.add_child(),
            MenuItem::AddRoot => self.add_root(),
            MenuItem::EditContents => self.edit_selected(),
            MenuItem::ToggleActive => self.toggle_active(),
            MenuItem::ToggleBookmark => self.toggle_bookmark(),
            MenuItem::NextBookmark => self.select_bookmark(1),
            MenuItem::PrevBookmark => self.select_bookmark(-1),
            MenuItem::SplitNode => self.split_selected(),
            MenuItem::MergeNode => self.merge_selected(),
            MenuItem::MoveNode => self.move_selected(),
            MenuItem::SortByContent => self.sort_selected(false),
            MenuItem::SortById => self.sort_selected(true),
            MenuItem::DeleteNode => self.delete_selected(),
            MenuItem::EditTitle => {
                self.open_input(InputPurpose::Title, self.document.metadata().to_owned());
            }
            MenuItem::PanZoom => self.view_mode = true,
            #[cfg(test)]
            MenuItem::NewDocument => self.request_new_document(),
            MenuItem::Save => {
                self.request_save(false);
            }
            MenuItem::ClearLog => {
                self.document.reset_action_count();
                self.status = "Reset the action counters".to_owned();
            }
            MenuItem::Help => self.dialog = Some(Dialog::Help(0)),
            MenuItem::Exit => self.request_exit(),
        }
    }

    fn open_input(&mut self, purpose: InputPurpose, value: String) {
        self.dialog = Some(Dialog::Keyboard(KeyboardDialog::new(purpose, value)));
    }

    fn submit_input(&mut self, purpose: InputPurpose, value: String) {
        match purpose {
            InputPurpose::Title => {
                self.document.set_metadata(value);
                self.status = "Updated the document title".to_owned();
                self.mark_dirty();
            }
            InputPurpose::EditNode(id) => {
                if self.document.apply_edit(&id, value) {
                    self.status = format!("Edited contents of #{id}");
                    self.mark_dirty();
                    self.refresh_graph(false);
                } else {
                    self.status = format!("Could not edit missing node #{id}");
                }
            }
            InputPurpose::SplitNode(id) => match value.trim().parse::<usize>() {
                Ok(at) if self.document.split(&id, at, self.next_id) => {
                    let new_id = self.next_id;
                    self.advance_id();
                    self.status = format!("Split #{id} at byte {at}; tail became #{new_id}");
                    self.mark_dirty();
                    self.refresh_graph(true);
                }
                Ok(at) => self.status = format!("Could not split #{id} at byte {at}"),
                Err(_) => self.status = format!("Invalid byte offset: {value:?}"),
            },
            InputPurpose::MoveNode(id) => {
                let parents: Result<Vec<u64>, _> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::parse)
                    .collect();
                match parents {
                    Ok(parents) => match self.document.move_node(&id, &parents) {
                        Ok(()) => {
                            self.status = format!("Moved #{id} under {parents:?}");
                            self.mark_dirty();
                            self.refresh_graph(true);
                        }
                        Err(error) => self.status = error,
                    },
                    Err(_) => self.status = format!("Invalid parent list: {value:?}"),
                }
            }
        }
    }

    fn request_save(&mut self, exit_after: bool) {
        self.status = "Saving...".into();
        self.event = AppEvent::Save { exit_after };
    }

    pub fn save_succeeded(&mut self, exit_after: bool) -> AppEvent {
        self.dirty = false;
        self.dialog = None;
        if exit_after {
            self.new_after_save = false;
            self.status = "Saved".into();
            AppEvent::Close
        } else if self.new_after_save {
            self.new_after_save = false;
            self.open_new_document_chooser();
            self.status = "Saved; choose the new document kind".into();
            AppEvent::None
        } else {
            self.status = "Saved".into();
            AppEvent::None
        }
    }

    pub fn save_failed(&mut self, exit_after: bool, error: String) {
        self.status = error;
        if self.new_after_save {
            self.new_after_save = false;
            self.dialog = Some(Dialog::ConfirmNewDocument(NewDocumentChoice::Save));
        } else if exit_after {
            self.dialog = Some(Dialog::ConfirmExit(ExitChoice::Save));
        }
    }

    pub fn set_status(&mut self, status: String) {
        self.status = status;
    }

    #[cfg(test)]
    fn request_new_document(&mut self) {
        if self.dirty {
            self.dialog = Some(Dialog::ConfirmNewDocument(NewDocumentChoice::Save));
        } else {
            self.open_new_document_chooser();
        }
    }

    fn open_new_document_chooser(&mut self) {
        self.dialog = Some(Dialog::NewDocument {
            kind: self.document.kind(),
            startup: false,
        });
    }

    fn advance_id(&mut self) {
        self.next_id = self.next_id.saturating_add(1);
    }

    fn add_root(&mut self) {
        let id = self.next_id;
        if self.document.add_root(id) {
            self.advance_id();
            self.selected = Some(id);
            self.status = format!("Added root node #{id}");
            self.mark_dirty();
            self.refresh_graph(true);
        } else {
            self.status = format!("Failed to add root node #{id}");
        }
    }

    fn add_child(&mut self) {
        let Some(parent) = self.selected else {
            self.status = "Select a parent node first".to_owned();
            return;
        };
        let id = self.next_id;
        if self.document.add_child(&parent, id) {
            self.advance_id();
            self.selected = Some(id);
            self.status = format!("Added child #{id} under #{parent}");
            self.mark_dirty();
            self.refresh_graph(true);
        } else {
            self.status = format!("Failed to add a child under #{parent}");
        }
    }

    fn edit_selected(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node to edit".to_owned();
            return;
        };
        if let Some(contents) = self.document.node_contents(&id) {
            self.open_input(InputPurpose::EditNode(id), contents.to_owned());
        }
    }

    fn toggle_active(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node first".to_owned();
            return;
        };
        if self.document.toggle_active(&id) {
            self.status = format!("Toggled active state of #{id}");
            self.mark_dirty();
        }
    }

    fn toggle_bookmark(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node first".to_owned();
            return;
        };
        if let Some(info) = self.document.node_info(&id) {
            if self.document.set_bookmarked(&id, !info.bookmarked) {
                self.status = if info.bookmarked {
                    format!("Removed bookmark from #{id}")
                } else {
                    format!("Bookmarked #{id}")
                };
                self.mark_dirty();
                self.refresh_graph(false);
            }
        }
    }

    fn split_selected(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node to split".to_owned();
            return;
        };
        let Some(contents) = self.document.node_contents(&id) else {
            return;
        };
        if contents.len() < 2 {
            self.status = format!("Node #{id} is too short to split");
            return;
        }
        let mut at = contents.len() / 2;
        while at > 0 && !contents.is_char_boundary(at) {
            at -= 1;
        }
        if at == 0 {
            at = contents.chars().next().map_or(0, char::len_utf8);
        }
        self.open_input(InputPurpose::SplitNode(id), at.to_string());
    }

    fn merge_selected(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node to merge".to_owned();
            return;
        };
        match self.document.merge_with_parent(&id) {
            Some(parent) => {
                self.selected = Some(parent);
                self.status = format!("Merged #{id} into parent #{parent}");
                self.mark_dirty();
                self.refresh_graph(true);
            }
            None => self.status = format!("Could not merge #{id} with a parent"),
        }
    }

    fn move_selected(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node to move".to_owned();
            return;
        };
        if self.document.kind() != WeaveKind::Independent {
            self.status = "Move is only available for IndependentWeave documents".to_owned();
            return;
        }
        let parents = self
            .document
            .node_info(&id)
            .map(|info| {
                info.parents
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        self.open_input(InputPurpose::MoveNode(id), parents);
    }

    fn sort_selected(&mut self, by_id: bool) {
        let Some(id) = self.selected else {
            self.status = "Select a parent node to sort".to_owned();
            return;
        };
        let sorted = if by_id {
            self.document.sort_children_by_id(&id)
        } else {
            self.document.sort_children(&id)
        };
        if sorted {
            self.status = if by_id {
                format!("Sorted children of #{id} by ID")
            } else {
                format!("Sorted children of #{id} by contents")
            };
            self.mark_dirty();
            self.refresh_graph(true);
        } else {
            self.status = format!("Could not sort children of #{id}");
        }
    }

    fn delete_selected(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node to delete".to_owned();
            return;
        };
        match self.document.remove(&id) {
            Some(count) => {
                self.selected = None;
                self.status = format!("Removed {count} node(s) starting at #{id}");
                self.mark_dirty();
                self.refresh_graph(true);
            }
            None => self.status = format!("Failed to remove #{id}"),
        }
    }

    fn select_direction(&mut self, direction: NavigationDirection) {
        if self.nodes.is_empty() {
            self.selected = None;
            return;
        }
        let Some(selected) = self.selected else {
            if let Some(node) = self.nodes.first() {
                self.select(node.id);
            }
            return;
        };
        if self.layout.node_center(selected).is_none() {
            self.selected = None;
            return;
        }
        if let Some(id) = self.layout.directional_neighbor(selected, direction) {
            self.select(id);
        } else {
            let direction = match direction {
                NavigationDirection::Left => "left",
                NavigationDirection::Down => "below",
                NavigationDirection::Up => "above",
                NavigationDirection::Right => "right",
            };
            self.status = format!("No node {direction} of #{selected}");
        }
    }

    fn select_bookmark(&mut self, direction: isize) {
        let bookmarks = self.document.bookmarks();
        if bookmarks.is_empty() {
            self.status = "There are no bookmarks".to_owned();
            return;
        }
        let current = self
            .selected
            .and_then(|id| bookmarks.iter().position(|bookmark| *bookmark == id));
        let index = match (current, direction.is_negative()) {
            (None, false) => 0,
            (None, true) => bookmarks.len() - 1,
            (Some(index), false) => (index + 1) % bookmarks.len(),
            (Some(index), true) => index.checked_sub(1).unwrap_or(bookmarks.len() - 1),
        };
        let id = bookmarks[index];
        if self.document.set_active(&id) {
            self.mark_dirty();
        }
        self.select(id);
        self.status = format!("Jumped to bookmark #{id}");
    }

    fn select(&mut self, id: u64) {
        self.selected = Some(id);
        self.viewport.focus(&self.layout, id);
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let path = self.document.active_path();
        let path_set: HashSet<u64> = path.iter().copied().collect();
        let path_edges = tree_view::active_path_edges(&path);
        let active = self.document.active_set();

        let reading = self.compact_tab == CompactTab::Reading;
        let constraints = |panel| {
            [
                Constraint::Length(1),
                Constraint::Min(8),
                panel,
                Constraint::Length(1),
                Constraint::Length(1),
            ]
        };
        let panel_constraint = if reading {
            Constraint::Percentage(50)
        } else {
            Constraint::Length(7)
        };
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints(panel_constraint))
            .split(area);
        // The expanded reading panel steals rows from the tree; crop the tree
        // to the rows it kept instead of squeezing the whole graph into them.
        let vertical_crop = if reading {
            let reference = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints(Constraint::Length(7)))
                .split(area);
            canvas_rows(vertical[1]) / canvas_rows(reference[1])
        } else {
            1.0
        };
        self.render_top_bar(frame, vertical[0]);

        let tree = TreeView {
            nodes: &self.nodes,
            layout: &self.layout,
            viewport: self.viewport,
            selected: self.selected,
            active: &active,
            path: &path_set,
            path_edges: &path_edges,
            vertical_crop,
        };
        let hint = if self.view_mode { " pan/zoom " } else { "" };
        tree_view::render(frame, vertical[1], tree, hint);
        self.render_compact_panel(frame, vertical[2], &path);

        frame.render_widget(
            Paragraph::new(self.status.as_str()).style(Style::default().fg(Color::Yellow)),
            vertical[3],
        );
        self.render_bottom_bar(frame, vertical[4]);

        if let Some(dialog) = self.dialog.take() {
            self.render_dialog(frame, &dialog);
            self.dialog = Some(dialog);
        }
    }

    fn render_top_bar(&self, frame: &mut Frame, area: Rect) {
        let area = Rect {
            width: area.width.saturating_sub(8),
            ..area
        };
        let file = self.file_name.as_str();
        let marker = if self.dirty { "*" } else { "" };
        let kind = match self.document.kind() {
            WeaveKind::Dependent => "tree",
            WeaveKind::Independent => "DAG",
        };
        let line = Line::from(vec![
            Span::styled(
                format!(" {file}{marker} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " {}  {kind}  {} nodes",
                    self.document.metadata(),
                    self.document.len()
                ),
                Style::default().fg(Color::Gray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_bottom_bar(&self, frame: &mut Frame, area: Rect) {
        let hints = match &self.dialog {
            None if self.view_mode => "Joy:Pan Press:Fit 1:Zoom+ 2:Zoom- 3:Back",
            None if self.compact_tab == CompactTab::Reading => {
                "1:ScrlUp 2:Panel 3:ScrlDn Joy:Select Press:Active"
            }
            None => "1:Menu 2:Panel 3:Exit Joy:Select Press:Active",
            Some(Dialog::Help(_)) => "Joy:Scroll Press:Close",
            Some(Dialog::Menu(_)) => "Joy:Choose Press:Run 1/3:Close",
            Some(Dialog::NewDocument { .. }) => "Joy:Choose Press:Create 3:Cancel",
            Some(Dialog::ConfirmNewDocument(_)) => "Joy:Choose Press:Confirm 3:Cancel",
            Some(Dialog::ConfirmExit(_)) => "Joy:Choose Press:Confirm 3:Cancel",
            Some(Dialog::Keyboard(entry)) if entry.keyboard_visible => {
                "Joy:Key Press:Type 1:Cancel 2:Hide 3:Apply"
            }
            Some(Dialog::Keyboard(_)) => "Joy:Cursor Press:Keyboard 1:Cancel 2:Show 3:Apply",
        };
        frame.render_widget(
            Paragraph::new(hints).style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            area,
        );
    }

    fn render_compact_panel(&mut self, frame: &mut Frame, area: Rect, path: &[u64]) {
        let tabs = Tabs::new(CompactTab::ALL.map(CompactTab::title))
            .select(self.compact_tab.index())
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(" ")
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(tabs, area);
        let inner = Rect {
            x: area.x + 1,
            y: area.y + 2,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(3),
        };
        match self.compact_tab {
            CompactTab::Inspector => self.render_inspector_content(frame, inner),
            CompactTab::Bookmarks => self.render_bookmark_content(frame, inner),
            CompactTab::Reading => self.render_reading_content(frame, inner, path),
        }
    }

    fn render_inspector_content(&self, frame: &mut Frame, area: Rect) {
        let Some(id) = self.selected else {
            frame.render_widget(Paragraph::new("No node selected. Use the joystick."), area);
            return;
        };
        let Some(info) = self.document.node_info(&id) else {
            return;
        };
        let parents = id_list(&info.parents, "root");
        let children = id_list(&info.children, "none");
        let contents = self.document.node_contents(&id).unwrap_or_default();
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("Node #{id}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    "  active: {}  mark: {}  {} bytes",
                    yes_no(info.active),
                    yes_no(info.bookmarked),
                    info.content_len
                )),
            ]),
            Line::from(format!("Parents: {parents}  Children: {children}")),
        ];
        lines.extend(contents.lines().map(Line::from));
        frame.render_widget(
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_bookmark_content(&self, frame: &mut Frame, area: Rect) {
        let bookmarks = self.document.bookmarks();
        if bookmarks.is_empty() {
            frame.render_widget(Paragraph::new("No bookmarks yet."), area);
            return;
        }
        let items = bookmarks.into_iter().map(|id| {
            let contents = self.document.node_contents(&id).unwrap_or_default();
            let snippet: String = contents
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect();
            let style = if self.selected == Some(id) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(format!("#{id} {snippet}")).style(style)
        });
        frame.render_widget(List::new(items), area);
    }

    fn render_reading_content(&mut self, frame: &mut Frame, area: Rect, path: &[u64]) {
        if path.is_empty() {
            self.reading_scroll = 0;
            frame.render_widget(
                Paragraph::new("No active path. Toggle a node active."),
                area,
            );
            return;
        }
        let breadcrumb = path
            .iter()
            .rev()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(" > ");
        let mut lines = vec![Line::styled(
            breadcrumb,
            Style::default().fg(Color::DarkGray),
        )];
        let mut contents = String::new();
        for id in path.iter().rev() {
            if let Some(node_contents) = self.document.node_contents(id) {
                contents.push_str(node_contents);
            }
        }
        lines.extend(contents.lines().map(Line::from));
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let total = u16::try_from(paragraph.line_count(area.width)).unwrap_or(u16::MAX);
        self.reading_scroll = self.reading_scroll.min(total.saturating_sub(area.height));
        frame.render_widget(paragraph.scroll((self.reading_scroll, 0)), area);
    }

    fn render_dialog(&self, frame: &mut Frame, dialog: &Dialog) {
        match dialog {
            Dialog::Help(scroll) => self.render_help(frame, *scroll),
            Dialog::Menu(index) => self.render_menu(frame, *index),
            Dialog::NewDocument { kind, .. } => self.render_new_document(frame, *kind),
            Dialog::ConfirmNewDocument(choice) => self.render_confirm_new_document(frame, *choice),
            Dialog::ConfirmExit(choice) => self.render_confirm_exit(frame, *choice),
            Dialog::Keyboard(keyboard) => self.render_keyboard(frame, &keyboard),
        }
    }

    fn render_help(&self, frame: &mut Frame, scroll: u16) {
        let area = centered(frame.area(), 51, 22);
        frame.render_widget(Clear, area);
        let text = Text::from(vec![
            Line::styled("JOYSTICK", Style::default().fg(Color::Cyan)),
            Line::from("move: select node    press: toggle active"),
            Line::from(""),
            Line::styled("BUTTONS", Style::default().fg(Color::Cyan)),
            Line::from("1: menu    2: next panel    3: exit"),
            Line::from("Reading panel: 1/3 scroll, 2 leaves it."),
            Line::from(""),
            Line::styled("MENU", Style::default().fg(Color::Cyan)),
            Line::from("All node and document actions live in the"),
            Line::from("menu: joystick up/down, press to run."),
            Line::from(""),
            Line::styled("PAN/ZOOM VIEW", Style::default().fg(Color::Cyan)),
            Line::from("joystick pans, 1/2 zoom, press fits, 3 back."),
            Line::from(""),
            Line::styled("ON-SCREEN KEYBOARD", Style::default().fg(Color::Cyan)),
            Line::from("joystick picks a key, press types it."),
            Line::from("1: cancel  2: hide/show keyboard  3: apply"),
            Line::from("Hidden: left/right move the text cursor."),
            Line::from(""),
            Line::styled("EXIT", Style::default().fg(Color::Cyan)),
            Line::from("Prompts to save when there are unsaved"),
            Line::from("changes. Other actions apply immediately."),
        ]);
        frame.render_widget(
            Paragraph::new(text).scroll((scroll, 0)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Help ")
                    .title_bottom(" any button closes "),
            ),
            area,
        );
    }

    fn render_menu(&self, frame: &mut Frame, index: usize) {
        let items = self.menu_items();
        let height = (items.len() as u16).saturating_add(2);
        let area = centered(frame.area(), 30, height);
        frame.render_widget(Clear, area);
        let inner_height = usize::from(area.height.saturating_sub(2)).max(1);
        let offset = index.saturating_sub(inner_height - 1);
        let lines: Vec<ListItem> = items
            .iter()
            .enumerate()
            .skip(offset)
            .take(inner_height)
            .map(|(position, item)| {
                let style = if position == index {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(item.label()).style(style)
            })
            .collect();
        frame.render_widget(
            List::new(lines).block(Block::default().borders(Borders::ALL).title(" Menu ")),
            area,
        );
    }

    fn render_new_document(&self, frame: &mut Frame, kind: WeaveKind) {
        let area = centered(frame.area(), 46, 7);
        frame.render_widget(Clear, area);
        let choices = Line::from(vec![
            Span::styled(" Dependent ", choice_style(kind == WeaveKind::Dependent)),
            Span::raw("   "),
            Span::styled(
                " Independent ",
                choice_style(kind == WeaveKind::Independent),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from("Choose the weave implementation:"),
                Line::from(""),
                choices,
            ]))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" New document "),
            ),
            area,
        );
    }

    fn render_confirm_exit(&self, frame: &mut Frame, choice: ExitChoice) {
        let area = centered(frame.area(), 34, 8);
        frame.render_widget(Clear, area);
        let mut lines = vec![Line::from("There are unsaved changes."), Line::from("")];
        lines.extend(ExitChoice::ALL.iter().map(|option| {
            Line::styled(
                format!(" {} ", option.label()),
                choice_style(*option == choice),
            )
        }));
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title(" Exit ")),
            area,
        );
    }

    fn render_confirm_new_document(&self, frame: &mut Frame, choice: NewDocumentChoice) {
        let area = centered(frame.area(), 38, 8);
        frame.render_widget(Clear, area);
        let mut lines = vec![
            Line::from("The current document has changes."),
            Line::from(""),
        ];
        lines.extend(NewDocumentChoice::ALL.iter().map(|option| {
            Line::styled(
                format!(" {} ", option.label()),
                choice_style(*option == choice),
            )
        }));
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" New document "),
                ),
            area,
        );
    }

    fn render_keyboard(&self, frame: &mut Frame, keyboard: &KeyboardDialog) {
        let multiline = keyboard.purpose.multiline();
        let text_height: u16 = if multiline { 4 } else { 1 };
        let area = centered(frame.area(), 43, text_height + 7);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(keyboard.purpose.title());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(text_height),
                Constraint::Length(1),
                Constraint::Length(4),
            ])
            .split(inner);

        let (line, column) = keyboard.buffer.cursor_line_column();
        let visible_height = usize::from(sections[0].height.max(1));
        let visible_width = usize::from(sections[0].width.max(1));
        let scroll_y = line.saturating_sub(visible_height.saturating_sub(1));
        let scroll_x = column.saturating_sub(visible_width.saturating_sub(1));
        // Render the insertion point as ordinary buffered text, just like the
        // filesystem manager's name editor. The embedded backend paints its
        // terminal cursor as an underscore outside ratatui's cell buffer, so
        // moving that cursor can leave stale underscores that later frames do
        // not know they need to erase.
        let mut text_with_cursor = keyboard.buffer.text.clone();
        text_with_cursor.insert(keyboard.buffer.cursor, '|');
        frame.render_widget(
            Paragraph::new(text_with_cursor).scroll((scroll_y as u16, scroll_x as u16)),
            sections[0],
        );
        frame.render_widget(
            Paragraph::new(Line::styled(
                "-".repeat(usize::from(sections[1].width)),
                Style::default().fg(Color::DarkGray),
            )),
            sections[1],
        );

        if keyboard.keyboard_visible {
            let mut grid_lines = Vec::with_capacity(4);
            for row in 0..4 {
                let mut spans = Vec::new();
                for column in 0..keyboard.keyboard.row_len(row) {
                    let label = keyboard.keyboard.label(row, column);
                    let width = if row < 3 { 3 } else { 5 };
                    let selected =
                        keyboard.keyboard.row == row && keyboard.keyboard.column == column;
                    spans.push(Span::styled(
                        format!("{label:^width$}"),
                        key_style(selected),
                    ));
                }
                grid_lines.push(Line::from(spans).alignment(Alignment::Center));
            }
            frame.render_widget(Paragraph::new(Text::from(grid_lines)), sections[2]);
        } else {
            frame.render_widget(
                Paragraph::new("Keyboard hidden; press joystick to show")
                    .alignment(Alignment::Center),
                sections[2],
            );
        }
    }
}

fn key_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    }
}

/// Rows the tree canvas draws into once its border is removed.
fn canvas_rows(area: Rect) -> f64 {
    f64::from(area.height.saturating_sub(2).max(1))
}

fn centered(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = max_width.min(area.width.saturating_sub(2)).max(1);
    let height = max_height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn id_list(ids: &[u64], empty: &str) -> String {
    if ids.is_empty() {
        empty.to_owned()
    } else {
        ids.iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn choice_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    }
}

#[cfg(test)]
mod tests {
    use super::super::document::seeded_dependent;
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const GRID_WIDTH: u16 = 53;
    const GRID_HEIGHT: u16 = 24;

    fn seeded_app() -> WeaveApp {
        WeaveApp::with_document(seeded_dependent(), "demo.UWE".into())
    }

    fn rendered(app: &mut WeaveApp) -> String {
        let backend = TestBackend::new(GRID_WIDTH, GRID_HEIGHT);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut output = String::new();
        for y in 0..GRID_HEIGHT {
            for x in 0..GRID_WIDTH {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }

    #[test]
    fn text_buffer_edits_unicode_at_char_boundaries() {
        let mut buffer = TextBuffer::new("hé".to_owned());
        buffer.left();
        buffer.insert('🦄');
        assert_eq!(buffer.text, "h🦄é");
        buffer.backspace();
        assert_eq!(buffer.text, "hé");
        buffer.left();
        buffer.delete();
        assert_eq!(buffer.text, "é");
        assert!(buffer.text.is_char_boundary(buffer.cursor));
    }

    #[test]
    fn menu_actions_mutate_document_and_selection() {
        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::AddChild);
        assert_eq!(app.selected, Some(5));
        assert!(app.document.contains(&5));
        app.execute_menu_item(MenuItem::ToggleBookmark);
        assert!(app.document.node_info(&5).unwrap().bookmarked);
        app.execute_menu_item(MenuItem::DeleteNode);
        assert!(!app.document.contains(&5));
        assert_eq!(app.selected, None);
        assert!(app.dirty);
    }

    #[test]
    fn menu_opens_navigates_and_runs_items() {
        let mut app = seeded_app();
        assert!(!app.menu_items().contains(&MenuItem::NewDocument));
        app.handle_input(Input::Button1);
        assert_eq!(app.dialog, Some(Dialog::Menu(0)));
        app.handle_input(Input::Up);
        assert_eq!(app.dialog, Some(Dialog::Menu(app.menu_items().len() - 1)));
        app.handle_input(Input::Down);
        assert_eq!(app.dialog, Some(Dialog::Menu(0)));
        // First item is AddChild.
        app.handle_input(Input::Press);
        assert!(app.dialog.is_none());
        assert!(app.document.contains(&5));
    }

    #[test]
    fn joystick_navigates_in_coordinate_space() {
        let mut app = seeded_app();
        let [_, first_y] = app.layout.node_center(1).unwrap();
        let [_, second_y] = app.layout.node_center(2).unwrap();
        let (upper, lower) = if first_y > second_y { (1, 2) } else { (2, 1) };

        app.select(upper);
        app.handle_input(Input::Down);
        assert_eq!(app.selected, Some(lower));
        app.handle_input(Input::Up);
        assert_eq!(app.selected, Some(upper));

        app.handle_input(Input::Left);
        assert_eq!(app.selected, Some(0));
        app.handle_input(Input::Right);
        assert!(matches!(app.selected, Some(1 | 2)));
    }

    #[test]
    fn keyboard_dialog_types_applies_and_cancels() {
        let mut app = seeded_app();
        app.selected = Some(3);
        app.execute_menu_item(MenuItem::EditContents);
        assert!(matches!(app.dialog, Some(Dialog::Keyboard(_))));

        // Backspace strips the trailing character, then type "q" (top-left
        // key on the filesystem manager's keyboard) and apply with button 3.
        let Some(Dialog::Keyboard(entry)) = &mut app.dialog else {
            panic!("expected keyboard dialog");
        };
        entry.keyboard.row = 3;
        entry.keyboard.column = 3;
        app.handle_input(Input::Press);
        let Some(Dialog::Keyboard(entry)) = &mut app.dialog else {
            panic!("expected keyboard dialog");
        };
        entry.keyboard.row = 0;
        entry.keyboard.column = 0;
        app.handle_input(Input::Press);
        app.handle_input(Input::Button3);
        assert!(app.dialog.is_none());
        let contents = app.document.node_contents(&3).unwrap();
        assert!(contents.ends_with('q'));
        assert!(app.dirty);

        // Button 1 cancels without applying, matching the name editor.
        app.dirty = false;
        app.execute_menu_item(MenuItem::EditContents);
        app.handle_input(Input::Button1);
        assert!(app.dialog.is_none());
        assert!(!app.dirty);
    }

    #[test]
    fn keyboard_charset_pages_cycle_and_split_opens_on_digits() {
        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::SplitNode);
        let Some(Dialog::Keyboard(entry)) = &app.dialog else {
            panic!("expected keyboard dialog");
        };
        assert_eq!(entry.keyboard.page, KeyboardPage::Symbols);
        let Some(Dialog::Keyboard(entry)) = &mut app.dialog else {
            panic!("expected keyboard dialog");
        };
        entry.keyboard.row = 3;
        entry.keyboard.column = 1;
        app.handle_input(Input::Press);
        let Some(Dialog::Keyboard(entry)) = &app.dialog else {
            panic!("expected keyboard dialog");
        };
        assert_eq!(entry.keyboard.page, KeyboardPage::Lower);
    }

    #[test]
    fn hidden_keyboard_moves_text_cursor_and_click_shows_it() {
        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::EditTitle);
        app.handle_input(Input::Button2);

        let Some(Dialog::Keyboard(entry)) = &app.dialog else {
            panic!("expected keyboard dialog");
        };
        assert!(!entry.keyboard_visible);
        let end = entry.buffer.cursor;

        app.handle_input(Input::Left);
        let Some(Dialog::Keyboard(entry)) = &app.dialog else {
            panic!("expected keyboard dialog");
        };
        assert!(entry.buffer.cursor < end);

        app.handle_input(Input::Press);
        let Some(Dialog::Keyboard(entry)) = &app.dialog else {
            panic!("expected keyboard dialog");
        };
        assert!(entry.keyboard_visible);
    }

    #[test]
    fn text_entry_uses_a_buffered_cursor_instead_of_the_terminal_cursor() {
        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::EditTitle);

        let output = rendered(&mut app);
        assert!(output.contains("The Lighthouse Letter|"));

        let backend = TestBackend::new(GRID_WIDTH, GRID_HEIGHT);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert!(!terminal.backend().cursor_visible());

        app.handle_input(Input::Button2);
        app.handle_input(Input::Left);
        let output = rendered(&mut app);
        assert!(output.contains("The Lighthouse Lette|r"));
    }

    #[test]
    fn exit_prompts_only_when_dirty() {
        let mut app = seeded_app();
        assert!(!app.dirty);
        assert_eq!(app.handle_input(Input::Button3), AppEvent::Close);

        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::ToggleActive);
        assert!(app.dirty);
        assert_eq!(app.handle_input(Input::Button3), AppEvent::None);
        assert_eq!(app.dialog, Some(Dialog::ConfirmExit(ExitChoice::Save)));

        // Cancel keeps the app running.
        app.handle_input(Input::Down);
        app.handle_input(Input::Down);
        app.handle_input(Input::Press);
        assert!(app.dialog.is_none());

        // Discard quits without saving.
        app.handle_input(Input::Button3);
        app.handle_input(Input::Down);
        assert_eq!(app.handle_input(Input::Press), AppEvent::Close);
    }

    #[test]
    fn save_and_exit_waits_for_persistence_result() {
        let mut app = WeaveApp::with_document(seeded_dependent(), "save.UWE".into());
        app.execute_menu_item(MenuItem::ToggleActive);
        app.request_exit();
        assert_eq!(
            app.handle_input(Input::Press),
            AppEvent::Save { exit_after: true }
        );
        assert!(app.dirty);
        app.save_failed(true, "write failed".into());
        assert!(matches!(app.dialog, Some(Dialog::ConfirmExit(_))));
        assert_eq!(app.save_succeeded(true), AppEvent::Close);
        assert!(!app.dirty);
    }

    #[test]
    fn dirty_new_document_requires_an_explicit_choice() {
        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::ToggleBookmark);
        let original_title = app.document.metadata().to_owned();

        app.execute_menu_item(MenuItem::NewDocument);
        assert_eq!(
            app.dialog,
            Some(Dialog::ConfirmNewDocument(NewDocumentChoice::Save))
        );

        // Button 3 cancels without touching the live document.
        app.handle_input(Input::Button3);
        assert!(app.dialog.is_none());
        assert_eq!(app.document.metadata(), original_title);
        assert!(app.dirty);

        // Discard is a separate choice and still does not replace the
        // document until the kind chooser is confirmed.
        app.execute_menu_item(MenuItem::NewDocument);
        app.handle_input(Input::Down);
        app.handle_input(Input::Press);
        assert!(matches!(
            app.dialog,
            Some(Dialog::NewDocument { startup: false, .. })
        ));
        assert_eq!(app.document.metadata(), original_title);
        app.handle_input(Input::Button3);
        assert_eq!(app.document.metadata(), original_title);
        assert!(app.dirty);

        app.execute_menu_item(MenuItem::NewDocument);
        app.handle_input(Input::Down);
        app.handle_input(Input::Press);
        app.handle_input(Input::Press);
        assert_eq!(app.document.metadata(), "Untitled document");
        assert!(app.dirty);
    }

    #[test]
    fn save_before_new_waits_for_persistence() {
        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::ToggleActive);
        let original_title = app.document.metadata().to_owned();

        app.execute_menu_item(MenuItem::NewDocument);
        assert_eq!(
            app.handle_input(Input::Press),
            AppEvent::Save { exit_after: false }
        );
        assert_eq!(app.document.metadata(), original_title);
        assert!(app.dirty);

        app.save_failed(false, "write failed".into());
        assert_eq!(
            app.dialog,
            Some(Dialog::ConfirmNewDocument(NewDocumentChoice::Save))
        );
        assert_eq!(app.document.metadata(), original_title);
        assert!(app.dirty);

        assert_eq!(
            app.handle_input(Input::Press),
            AppEvent::Save { exit_after: false }
        );
        assert_eq!(app.save_succeeded(false), AppEvent::None);
        assert!(matches!(
            app.dialog,
            Some(Dialog::NewDocument { startup: false, .. })
        ));
        assert_eq!(app.document.metadata(), original_title);
        assert!(!app.dirty);
    }

    #[test]
    fn startup_kind_chooser_creates_or_quits() {
        let mut app = WeaveApp::with_new_document("new.UWE".into());
        assert!(matches!(
            app.dialog,
            Some(Dialog::NewDocument { startup: true, .. })
        ));
        app.handle_input(Input::Right);
        app.handle_input(Input::Press);
        assert!(app.dialog.is_none());
        assert_eq!(app.document.kind(), WeaveKind::Independent);
        assert!(app.dirty);

        let mut app = WeaveApp::with_new_document("cancel.UWE".into());
        assert_eq!(app.handle_input(Input::Button3), AppEvent::Close);
    }

    #[test]
    fn view_mode_pans_zooms_and_exits() {
        let mut app = seeded_app();
        app.execute_menu_item(MenuItem::PanZoom);
        assert!(app.view_mode);
        let before = app.viewport;
        app.handle_input(Input::Right);
        assert_ne!(app.viewport.center, before.center);
        app.handle_input(Input::Button1);
        assert!(app.viewport.zoom > 1.0);
        app.handle_input(Input::Press);
        assert_eq!(app.viewport.zoom, 1.0);
        app.handle_input(Input::Button3);
        assert!(!app.view_mode);
    }

    #[test]
    fn layout_renders_top_and_bottom_bars() {
        let mut app = seeded_app();
        let output = rendered(&mut app);
        assert!(output.contains("demo.UWE"));
        assert!(output.contains("The Lighthouse Letter"));
        assert!(output.contains("Inspector"));
        assert!(output.contains("1:Menu 2:Panel 3:Exit"));
        assert!(output.lines().next().unwrap().ends_with("        "));

        app.execute_menu_item(MenuItem::ToggleActive);
        let output = rendered(&mut app);
        assert!(output.contains("demo.UWE*"));
    }

    #[test]
    fn dialogs_render_over_the_application() {
        let mut app = seeded_app();
        app.handle_input(Input::Button1);
        let output = rendered(&mut app);
        assert!(output.contains("Menu"));
        assert!(output.contains("Add child node"));

        app.handle_input(Input::Button1);
        app.execute_menu_item(MenuItem::EditContents);
        let output = rendered(&mut app);
        assert!(output.contains("Edit node contents"));
        assert!(output.contains("CASE"));
        assert!(output.contains("1:Cancel 2:Hide 3:Apply"));

        app.handle_input(Input::Button3);
        app.execute_menu_item(MenuItem::Help);
        let output = rendered(&mut app);
        assert!(output.contains("ON-SCREEN KEYBOARD"));

        app.handle_input(Input::Press);
        app.request_exit();
        let output = rendered(&mut app);
        assert!(output.contains("Save & exit"));
        assert!(output.contains("Discard & exit"));
    }

    #[test]
    fn reading_tab_scrolls_clamps_and_resets() {
        let mut app = seeded_app();
        app.handle_input(Input::Button2); // Bookmarks
        app.handle_input(Input::Button2); // Reading
        assert_eq!(app.compact_tab, CompactTab::Reading);

        app.handle_input(Input::Button3);
        app.handle_input(Input::Button3);
        assert_eq!(app.reading_scroll, 2);
        app.handle_input(Input::Button1);
        assert_eq!(app.reading_scroll, 1);

        // The joystick keeps selecting nodes rather than scrolling.
        app.handle_input(Input::Down);
        assert_eq!(app.reading_scroll, 1);

        // Rendering clamps the scroll to the wrapped content height.
        app.reading_scroll = u16::MAX;
        rendered(&mut app);
        assert!(app.reading_scroll < 10);

        // Cycling back to the first tab resets the scroll position.
        app.handle_input(Input::Button2);
        assert_eq!(app.compact_tab, CompactTab::Inspector);
        assert_eq!(app.reading_scroll, 0);
    }

    #[test]
    fn reading_tab_flows_adjacent_nodes_together() {
        let mut app = seeded_app();
        assert!(app.document.apply_edit(&0, "root ".to_owned()));
        assert!(app.document.apply_edit(&1, "middle ".to_owned()));
        assert!(app.document.apply_edit(&3, "tip".to_owned()));
        app.handle_input(Input::Button2); // Bookmarks
        app.handle_input(Input::Button2); // Reading

        let output = rendered(&mut app);

        assert!(output.contains("root middle tip"));
    }

    #[test]
    fn scroll_buttons_repeat_only_in_the_reading_tab() {
        let mut app = seeded_app();
        assert!(app.accepts_repeat(Input::Up));
        assert!(!app.accepts_repeat(Input::Button1));
        assert!(!app.accepts_repeat(Input::Button3));

        app.handle_input(Input::Button2); // Bookmarks
        app.handle_input(Input::Button2); // Reading
        assert!(app.accepts_repeat(Input::Button1));
        assert!(app.accepts_repeat(Input::Button3));
        assert!(!app.accepts_repeat(Input::Button2));
        assert!(!app.accepts_repeat(Input::Press));

        // Dialogs take the buttons back for their own actions.
        app.dialog = Some(Dialog::Menu(0));
        assert!(!app.accepts_repeat(Input::Button1));
        assert!(app.accepts_repeat(Input::Down));
    }

    #[test]
    fn move_node_menu_item_requires_independent_documents() {
        let app = seeded_app();
        assert!(!app.menu_items().contains(&MenuItem::MoveNode));
        let mut app = seeded_app();
        app.install_document(Document::empty(WeaveKind::Independent), true);
        assert!(app.menu_items().contains(&MenuItem::MoveNode));
    }
}
