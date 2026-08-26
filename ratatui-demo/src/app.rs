//! Application state, keyboard handling, and terminal UI.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::document::{Document, WeaveKind, seeded_dependent};
use crate::persistence;
use crate::tree_view::{self, GraphViewport, NavigationDirection, TreeLayout, TreeNode, TreeView};

const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 20;
const WIDE_WIDTH: u16 = 100;
const WIDE_HEIGHT: u16 = 30;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum CompactTab {
    #[default]
    Inspector,
    Bookmarks,
    Reading,
    Actions,
}

impl CompactTab {
    const ALL: [Self; 4] = [
        Self::Inspector,
        Self::Bookmarks,
        Self::Reading,
        Self::Actions,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Inspector => 0,
            Self::Bookmarks => 1,
            Self::Reading => 2,
            Self::Actions => 3,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Inspector => "Inspector",
            Self::Bookmarks => "Bookmarks",
            Self::Reading => "Reading",
            Self::Actions => "Actions",
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

    fn insert_str(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
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

    fn home(&mut self) {
        self.cursor = self.line_start();
    }

    fn end(&mut self) {
        self.cursor = self.line_end();
    }

    fn up(&mut self) {
        let start = self.line_start();
        if start == 0 {
            return;
        }
        let column = self.text[start..self.cursor].chars().count();
        let previous_end = start - 1;
        let previous_start = self.text[..previous_end]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.cursor = byte_at_char(&self.text, previous_start, previous_end, column);
    }

    fn down(&mut self) {
        let end = self.line_end();
        if end == self.text.len() {
            return;
        }
        let column = self.text[self.line_start()..self.cursor].chars().count();
        let next_start = end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map_or(self.text.len(), |offset| next_start + offset);
        self.cursor = byte_at_char(&self.text, next_start, next_end, column);
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

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |offset| self.cursor + offset)
    }
}

fn byte_at_char(text: &str, start: usize, end: usize, column: usize) -> usize {
    text[start..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(offset, _)| start + offset)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputDialog {
    purpose: InputPurpose,
    buffer: TextBuffer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Dialog {
    Help,
    NewDocument(WeaveKind),
    Input(InputDialog),
}

pub struct App {
    document: Document,
    next_id: u64,
    selected: Option<u64>,
    status: String,
    nodes: Vec<TreeNode>,
    layout: TreeLayout,
    viewport: GraphViewport,
    dialog: Option<Dialog>,
    compact_tab: CompactTab,
    current_path: Option<PathBuf>,
    reading_scroll: u16,
    action_scroll: u16,
    should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        let document = seeded_dependent();
        let mut app = Self {
            document,
            next_id: 5,
            selected: Some(3),
            status: "Welcome! Press ? for keyboard controls.".to_owned(),
            nodes: Vec::new(),
            layout: TreeLayout::default(),
            viewport: GraphViewport::default(),
            dialog: None,
            compact_tab: CompactTab::default(),
            current_path: None,
            reading_scroll: 0,
            action_scroll: 0,
            should_quit: false,
        };
        app.refresh_graph(true);
        app
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    self.handle_key(key);
                }
                Event::Paste(value) => self.handle_paste(&value),
                _ => {}
            }
        }
        Ok(())
    }

    fn refresh_graph(&mut self, fit: bool) {
        self.layout = self.document.tree_layout();
        self.nodes = self.document.tree_nodes();
        if fit {
            self.viewport.fit(&self.layout);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.dialog.is_some() {
            self.handle_dialog_key(key);
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.dialog = Some(Dialog::Help),
            KeyCode::Char('n') => {
                self.dialog = Some(Dialog::NewDocument(self.document.kind()));
            }
            KeyCode::Char('o') => self.open_document(),
            KeyCode::Char('s') => self.save_document(),
            KeyCode::Char('t') => {
                self.open_input(InputPurpose::Title, self.document.metadata().to_owned());
            }
            KeyCode::Char('L') => {
                self.document.clear_actions();
                self.status = "Cleared the action log".to_owned();
                self.action_scroll = 0;
            }
            KeyCode::Char('h') => self.select_direction(NavigationDirection::Left),
            KeyCode::Char('j') => self.select_direction(NavigationDirection::Down),
            KeyCode::Char('k') => self.select_direction(NavigationDirection::Up),
            KeyCode::Char('l') => self.select_direction(NavigationDirection::Right),
            KeyCode::Char('[') => self.select_bookmark(-1),
            KeyCode::Char(']') => self.select_bookmark(1),
            KeyCode::Left => self.viewport.pan(&self.layout, -0.1, 0.0),
            KeyCode::Right => self.viewport.pan(&self.layout, 0.1, 0.0),
            KeyCode::Up => self.viewport.pan(&self.layout, 0.0, 0.1),
            KeyCode::Down => self.viewport.pan(&self.layout, 0.0, -0.1),
            KeyCode::Char('+') | KeyCode::Char('=') => self.viewport.zoom_by(1.25),
            KeyCode::Char('-') => self.viewport.zoom_by(0.8),
            KeyCode::Char('0') => self.viewport.fit(&self.layout),
            KeyCode::Char('f') => {
                if let Some(id) = self.selected {
                    self.viewport.focus(&self.layout, id);
                }
            }
            KeyCode::Char('r') => self.add_root(),
            KeyCode::Char('a') => self.add_child(),
            KeyCode::Char('e') => self.edit_selected(),
            KeyCode::Char(' ') => self.toggle_active(),
            KeyCode::Char('b') => self.toggle_bookmark(),
            KeyCode::Char('x') => self.split_selected(),
            KeyCode::Char('M') => self.merge_selected(),
            KeyCode::Char('m') => self.move_selected(),
            KeyCode::Char('c') => self.sort_selected(false),
            KeyCode::Char('i') => self.sort_selected(true),
            KeyCode::Char('d') => self.delete_selected(),
            KeyCode::Tab => self.compact_tab = self.compact_tab.next(),
            KeyCode::PageUp => {
                self.reading_scroll = self.reading_scroll.saturating_sub(3);
                self.action_scroll = self.action_scroll.saturating_sub(3);
            }
            KeyCode::PageDown => {
                self.reading_scroll = self.reading_scroll.saturating_add(3);
                self.action_scroll = self.action_scroll.saturating_add(3);
            }
            _ => {}
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        match dialog {
            Dialog::Help => {
                if !matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.dialog = Some(Dialog::Help);
                }
            }
            Dialog::NewDocument(mut kind) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Left | KeyCode::Right | KeyCode::Char('d') | KeyCode::Char('i') => {
                    kind = match key.code {
                        KeyCode::Char('d') => WeaveKind::Dependent,
                        KeyCode::Char('i') => WeaveKind::Independent,
                        _ => match kind {
                            WeaveKind::Dependent => WeaveKind::Independent,
                            WeaveKind::Independent => WeaveKind::Dependent,
                        },
                    };
                    self.dialog = Some(Dialog::NewDocument(kind));
                }
                KeyCode::Enter => self.replace_document(
                    Document::empty(kind),
                    None,
                    format!("Created a new {} document", kind.label()),
                ),
                _ => self.dialog = Some(Dialog::NewDocument(kind)),
            },
            Dialog::Input(mut input) => {
                let submit = (input.purpose.multiline()
                    && key.code == KeyCode::Char('s')
                    && key.modifiers.contains(KeyModifiers::CONTROL))
                    || (!input.purpose.multiline() && key.code == KeyCode::Enter);
                if key.code == KeyCode::Esc {
                    return;
                }
                if submit {
                    self.submit_input(input.purpose, input.buffer.text);
                    return;
                }
                handle_text_key(&mut input.buffer, key, input.purpose.multiline());
                self.dialog = Some(Dialog::Input(input));
            }
        }
    }

    fn handle_paste(&mut self, value: &str) {
        if let Some(Dialog::Input(input)) = self.dialog.as_mut() {
            if input.purpose.multiline() {
                input.buffer.insert_str(value);
            } else {
                input.buffer.insert_str(&value.replace(['\r', '\n'], ""));
            }
        }
    }

    fn open_input(&mut self, purpose: InputPurpose, value: String) {
        self.dialog = Some(Dialog::Input(InputDialog {
            purpose,
            buffer: TextBuffer::new(value),
        }));
    }

    fn submit_input(&mut self, purpose: InputPurpose, value: String) {
        match purpose {
            InputPurpose::Title => {
                self.document.set_metadata(value);
                self.status = "Updated the document title".to_owned();
            }
            InputPurpose::EditNode(id) => {
                if self.document.apply_edit(&id, value) {
                    self.status = format!("Edited contents of #{id}");
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
                            self.refresh_graph(true);
                        }
                        Err(error) => self.status = error,
                    },
                    Err(_) => self.status = format!("Invalid parent list: {value:?}"),
                }
            }
        }
    }

    fn open_document(&mut self) {
        let mut dialog = rfd::FileDialog::new().add_filter("Universal Weave demo", &["uweave"]);
        if let Some(parent) = self.current_path.as_ref().and_then(|path| path.parent()) {
            dialog = dialog.set_directory(parent);
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        match persistence::load_document(&path) {
            Ok(document) => self.replace_document(
                document,
                Some(path.clone()),
                format!("Opened {}", path.display()),
            ),
            Err(error) => self.status = format!("Open failed: {error}"),
        }
    }

    fn save_document(&mut self) {
        let mut dialog = rfd::FileDialog::new().add_filter("Universal Weave demo", &["uweave"]);
        if let Some(path) = &self.current_path {
            if let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
            if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
                dialog = dialog.set_file_name(file_name);
            }
        } else {
            dialog = dialog.set_file_name("document.uweave");
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        match persistence::save_document(&path, &self.document) {
            Ok(()) => {
                self.current_path = Some(path.clone());
                self.status = format!("Saved {}", path.display());
            }
            Err(error) => self.status = format!("Save failed: {error}"),
        }
    }

    fn replace_document(&mut self, mut document: Document, path: Option<PathBuf>, status: String) {
        self.next_id = document.max_id().map_or(0, |id| id.saturating_add(1));
        self.selected = document.active_tip();
        self.document = document;
        self.current_path = path;
        self.status = status;
        self.reading_scroll = 0;
        self.action_scroll = 0;
        self.refresh_graph(true);
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
            self.open_input(InputPurpose::EditNode(id), contents);
        }
    }

    fn toggle_active(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node first".to_owned();
            return;
        };
        if self.document.toggle_active(&id) {
            self.status = format!("Toggled active state of #{id}");
        }
    }

    fn toggle_bookmark(&mut self) {
        let Some(id) = self.selected else {
            self.status = "Select a node first".to_owned();
            return;
        };
        if let Some(info) = self.document.node_info(&id)
            && self.document.set_bookmarked(&id, !info.bookmarked)
        {
            self.status = if info.bookmarked {
                format!("Removed bookmark from #{id}")
            } else {
                format!("Bookmarked #{id}")
            };
            self.refresh_graph(false);
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
        self.document.set_active(&id);
        self.select(id);
        self.status = format!("Jumped to bookmark #{id}");
    }

    fn select(&mut self, id: u64) {
        self.selected = Some(id);
        self.viewport.focus(&self.layout, id);
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(format!(
                    "Terminal is {}x{}. Resize to at least {MIN_WIDTH}x{MIN_HEIGHT}.",
                    area.width, area.height
                ))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Universal Weave "),
                ),
                area,
            );
            return;
        }

        let path = self.document.active_path();
        let path_set: HashSet<u64> = path.iter().copied().collect();
        let path_edges = tree_view::active_path_edges(&path);
        let active = self.document.active_set();

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(9),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        self.render_header(frame, vertical[0]);

        let tree = TreeView {
            nodes: &self.nodes,
            layout: &self.layout,
            viewport: self.viewport,
            selected: self.selected,
            active: &active,
            path: &path_set,
            path_edges: &path_edges,
        };
        let wide = area.width >= WIDE_WIDTH && area.height >= WIDE_HEIGHT;
        if wide {
            self.render_wide(frame, vertical[1], vertical[2], &path, tree);
        } else {
            tree_view::render(frame, vertical[1], tree);
            self.render_compact_panel(frame, vertical[2], &path);
        }

        frame.render_widget(
            Paragraph::new(self.status.as_str()).style(Style::default().fg(Color::Yellow)),
            vertical[3],
        );
        frame.render_widget(
            Paragraph::new(
                "? help  n new  o open  s save  h/j/k/l select  a child  e edit  Space active  q quit",
            )
            .style(Style::default().fg(Color::DarkGray)),
            vertical[4],
        );

        if let Some(dialog) = self.dialog.clone() {
            self.render_dialog(frame, dialog);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let file = self
            .current_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("unsaved");
        let line = Line::from(vec![
            Span::styled(
                self.document.metadata(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  ·  {}  ·  {} nodes  ·  {} actions  ·  {file}",
                self.document.kind().label(),
                self.document.len(),
                self.document.action_count(),
            )),
        ]);
        frame.render_widget(
            Paragraph::new(line)
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    fn render_wide(
        &self,
        frame: &mut Frame,
        main: Rect,
        bottom: Rect,
        path: &[u64],
        tree: TreeView<'_>,
    ) {
        let main_columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(72), Constraint::Percentage(28)])
            .split(main);
        tree_view::render(frame, main_columns[0], tree);
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(main_columns[1]);
        self.render_inspector(frame, sidebar[0]);
        self.render_bookmarks(frame, sidebar[1]);

        let bottom_columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(bottom);
        self.render_reading(frame, bottom_columns[0], path);
        self.render_actions(frame, bottom_columns[1]);
    }

    fn render_compact_panel(&self, frame: &mut Frame, area: Rect, path: &[u64]) {
        let tabs = Tabs::new(CompactTab::ALL.map(CompactTab::title))
            .select(self.compact_tab.index())
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(" · ")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Tab: next panel "),
            );
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
            CompactTab::Actions => self.render_action_content(frame, inner),
        }
    }

    fn render_inspector(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Block::default().borders(Borders::ALL).title(" Inspector "),
            area,
        );
        self.render_inspector_content(frame, area.inner(ratatui::layout::Margin::new(1, 1)));
    }

    fn render_inspector_content(&self, frame: &mut Frame, area: Rect) {
        let Some(id) = self.selected else {
            frame.render_widget(Paragraph::new("No node selected. Use j/k."), area);
            return;
        };
        let Some(info) = self.document.node_info(&id) else {
            return;
        };
        let parents = id_list(&info.parents, "root");
        let children = id_list(&info.children, "none");
        let contents = self.document.node_contents(&id).unwrap_or_default();
        let mut lines = vec![
            Line::styled(
                format!("Node #{id}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(format!("Parents: {parents}")),
            Line::from(format!("Children: {children}")),
            Line::from(format!(
                "Active: {}   Bookmark: {}",
                yes_no(info.active),
                yes_no(info.bookmarked)
            )),
            Line::from(format!("Length: {} bytes", info.content_len)),
            Line::from(""),
        ];
        lines.extend(contents.split('\n').map(|line| Line::from(line.to_owned())));
        let text = Text::from(lines);
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), area);
    }

    fn render_bookmarks(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Bookmarks  [ / ] "),
            area,
        );
        self.render_bookmark_content(frame, area.inner(ratatui::layout::Margin::new(1, 1)));
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
                .take(22)
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

    fn render_reading(&self, frame: &mut Frame, area: Rect, path: &[u64]) {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Reading view  PgUp/PgDn "),
            area,
        );
        self.render_reading_content(frame, area.inner(ratatui::layout::Margin::new(1, 1)), path);
    }

    fn render_reading_content(&self, frame: &mut Frame, area: Rect, path: &[u64]) {
        if path.is_empty() {
            frame.render_widget(
                Paragraph::new("No active path. Press Space on a node."),
                area,
            );
            return;
        }
        let breadcrumb = path
            .iter()
            .rev()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(" → ");
        let contents = path
            .iter()
            .rev()
            .filter_map(|id| self.document.node_contents(id))
            .collect::<String>();
        let mut lines = vec![Line::styled(breadcrumb, Style::default().fg(Color::DarkGray))];
        lines.extend(contents.split('\n').map(|line| Line::from(line.to_owned())));
        frame.render_widget(
            Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((self.reading_scroll, 0)),
            area,
        );
    }

    fn render_actions(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Action log  L: clear "),
            area,
        );
        self.render_action_content(frame, area.inner(ratatui::layout::Margin::new(1, 1)));
    }

    fn render_action_content(&self, frame: &mut Frame, area: Rect) {
        let actions = self.document.formatted_actions();
        let text = if actions.is_empty() {
            "No local actions.".to_owned()
        } else {
            actions.join("\n")
        };
        frame.render_widget(Paragraph::new(text).scroll((self.action_scroll, 0)), area);
    }

    fn render_dialog(&self, frame: &mut Frame, dialog: Dialog) {
        match dialog {
            Dialog::Help => self.render_help(frame),
            Dialog::NewDocument(kind) => self.render_new_document(frame, kind),
            Dialog::Input(input) => self.render_input(frame, &input),
        }
    }

    fn render_help(&self, frame: &mut Frame) {
        let area = centered(frame.area(), 82, 26);
        frame.render_widget(Clear, area);
        let text = Text::from(vec![
            Line::styled("DOCUMENT", Style::default().fg(Color::Cyan)),
            Line::from("n new   o open   s save   t title   L clear log   q quit"),
            Line::from(""),
            Line::styled("SELECT & VIEW", Style::default().fg(Color::Cyan)),
            Line::from("h/j/k/l select left/down/up/right   [/] bookmarks"),
            Line::from("arrows pan   +/- zoom   0 fit all   f focus selected"),
            Line::from("Tab cycles compact panels   PgUp/PgDn scroll text"),
            Line::from(""),
            Line::styled("EDIT SELECTED NODE", Style::default().fg(Color::Cyan)),
            Line::from("a add child   r add root   e edit   Space toggle active"),
            Line::from("b bookmark   x split   M merge   m move (Independent)"),
            Line::from("c sort children by content   i sort by ID   d delete"),
            Line::from(""),
            Line::styled("DIALOGS", Style::default().fg(Color::Cyan)),
            Line::from("Enter confirms single-line input; Ctrl+S applies content edits."),
            Line::from("Esc cancels. Changes and deletion take effect immediately."),
        ]);
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Keyboard help ")
                    .title_bottom(" Esc or ? to close "),
            ),
            area,
        );
    }

    fn render_new_document(&self, frame: &mut Frame, kind: WeaveKind) {
        let area = centered(frame.area(), 54, 7);
        frame.render_widget(Clear, area);
        let choices = Line::from(vec![
            Span::styled(" Dependent ", choice_style(kind == WeaveKind::Dependent)),
            Span::raw("     "),
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
                    .title(" New document ")
                    .title_bottom(" ←/→ or d/i  Enter create  Esc cancel "),
            ),
            area,
        );
    }

    fn render_input(&self, frame: &mut Frame, input: &InputDialog) {
        let height = if input.purpose.multiline() { 14 } else { 7 };
        let area = centered(frame.area(), 76, height);
        frame.render_widget(Clear, area);
        let inner = area.inner(ratatui::layout::Margin::new(1, 1));
        let (line, column) = input.buffer.cursor_line_column();
        let visible_height = usize::from(inner.height.max(1));
        let visible_width = usize::from(inner.width.max(1));
        let scroll_y = line.saturating_sub(visible_height.saturating_sub(1));
        let scroll_x = column.saturating_sub(visible_width.saturating_sub(1));
        let help = if input.purpose.multiline() {
            " Ctrl+S apply  Esc cancel "
        } else {
            " Enter confirm  Esc cancel "
        };
        frame.render_widget(
            Paragraph::new(input.buffer.text.as_str())
                .scroll((scroll_y as u16, scroll_x as u16))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(input.purpose.title())
                        .title_bottom(help),
                ),
            area,
        );
        frame.set_cursor_position((
            inner.x + column.saturating_sub(scroll_x) as u16,
            inner.y + line.saturating_sub(scroll_y) as u16,
        ));
    }
}

fn handle_text_key(buffer: &mut TextBuffer, key: KeyEvent, multiline: bool) {
    match key.code {
        KeyCode::Char(value)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            buffer.insert(value);
        }
        KeyCode::Enter if multiline => buffer.insert('\n'),
        KeyCode::Tab if multiline => buffer.insert('\t'),
        KeyCode::Backspace => buffer.backspace(),
        KeyCode::Delete => buffer.delete(),
        KeyCode::Left => buffer.left(),
        KeyCode::Right => buffer.right(),
        KeyCode::Up if multiline => buffer.up(),
        KeyCode::Down if multiline => buffer.down(),
        KeyCode::Home => buffer.home(),
        KeyCode::End => buffer.end(),
        _ => {}
    }
}

fn centered(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = max_width.min(area.width.saturating_sub(4)).max(1);
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
    if value { "yes" } else { "no" }
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
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn rendered(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut output = String::new();
        for y in 0..height {
            for x in 0..width {
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
        buffer.delete();
        assert_eq!(buffer.text, "h");
        assert!(buffer.text.is_char_boundary(buffer.cursor));
    }

    #[test]
    fn text_buffer_moves_between_lines() {
        let mut buffer = TextBuffer::new("one\ntwenty\ntri".to_owned());
        buffer.up();
        assert_eq!(&buffer.text[buffer.cursor..], "nty\ntri");
        buffer.up();
        assert_eq!(&buffer.text[buffer.cursor..], "\ntwenty\ntri");
        buffer.down();
        assert_eq!(&buffer.text[buffer.cursor..], "nty\ntri");
    }

    #[test]
    fn core_key_commands_mutate_document_and_selection() {
        let mut app = App::new();
        app.handle_key(key(KeyCode::Char('a')));
        assert_eq!(app.selected, Some(5));
        assert!(app.document.contains(&5));
        app.handle_key(key(KeyCode::Char('b')));
        assert!(app.document.node_info(&5).unwrap().bookmarked);
        app.handle_key(key(KeyCode::Char('d')));
        assert!(!app.document.contains(&5));
        assert_eq!(app.selected, None);
    }

    #[test]
    fn h_j_k_l_navigate_in_coordinate_space() {
        let mut app = App::new();
        let [_, first_y] = app.layout.node_center(1).unwrap();
        let [_, second_y] = app.layout.node_center(2).unwrap();
        let (upper, lower) = if first_y > second_y { (1, 2) } else { (2, 1) };

        app.select(upper);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, Some(lower));
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, Some(upper));

        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.selected, Some(0));
        app.handle_key(key(KeyCode::Char('l')));
        assert!(matches!(app.selected, Some(1 | 2)));
    }

    #[test]
    fn modal_cancel_and_apply_are_distinct() {
        let mut app = App::new();
        app.handle_key(key(KeyCode::Char('e')));
        assert!(matches!(app.dialog, Some(Dialog::Input(_))));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.dialog.is_none());

        app.open_input(InputPurpose::Title, "New title".to_owned());
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.document.metadata(), "New title");
    }

    #[test]
    fn wide_compact_and_undersized_layouts_render() {
        let mut app = App::new();
        let wide = rendered(&mut app, 120, 40);
        assert!(wide.contains("The Lighthouse Letter"));
        assert!(wide.contains("Inspector"));
        assert!(wide.contains("Reading view"));
        assert!(wide.contains("Action log"));

        let compact = rendered(&mut app, 80, 24);
        assert!(compact.contains("Tab: next panel"));
        assert!(compact.contains("Inspector"));

        let undersized = rendered(&mut app, 50, 15);
        assert!(undersized.contains("Resize to at least 60x20"));
    }

    #[test]
    fn help_and_input_dialogs_render_over_the_application() {
        let mut app = App::new();
        app.handle_key(key(KeyCode::Char('?')));
        assert!(rendered(&mut app, 100, 32).contains("Keyboard help"));
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Char('e')));
        let edit = rendered(&mut app, 100, 32);
        assert!(edit.contains("Edit node contents"));
        assert!(edit.contains("Ctrl+S apply"));
    }
}
