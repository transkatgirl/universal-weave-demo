//! A document: either weave implementation plus its action log, behind a single API.

use std::collections::HashSet;

use universal_weave::indexmap::IndexSet;
use universal_weave::loro::ExportMode;
use universal_weave::wrappers::{LoggedWeave, WeaveAction};
use universal_weave::{
    ActivePathWeave, ActiveSingularWeave, BookmarkableWeave, DiscreteWeave, IndependentWeave as _,
    MetadataWeave, Node, SemiIndependentWeave, SortableWeave, Weave,
};

use crate::content::{
    CollaborativeDemoWeave, DemoNode, DemoWeave, IndependentDemoNode, IndependentDemoWeave,
    TextContent,
};
use crate::tree_view::TreeNode;

/// The number of log entries shown in the action log panel.
const MAX_SHOWN_ACTIONS: usize = 50;

/// The weave implementations a document can be built on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WeaveKind {
    /// Tree-based weave; each node depends on the contents of its parent.
    #[default]
    Dependent,
    /// DAG-based weave; nodes do not depend on parent contents and can have multiple parents.
    Independent,
    /// Tree-based collaborative weave backed by a Loro CRDT document.
    DependentLoro,
}

impl WeaveKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dependent => "DependentWeave (tree)",
            Self::Independent => "IndependentWeave (DAG)",
            Self::DependentLoro => "DependentLoroWeave (collaborative tree)",
        }
    }
}

type LoggedDependent = LoggedWeave<DemoWeave, u64, DemoNode, TextContent, String>;
type LoggedIndependent =
    LoggedWeave<IndependentDemoWeave, u64, IndependentDemoNode, TextContent, String>;
type LoggedCollaborative = LoggedWeave<CollaborativeDemoWeave, u64, DemoNode, TextContent, String>;

/// A document wraps one weave implementation, plus its `LoggedWeave` action log.
///
/// Variants are boxed to keep the enum small regardless of implementation size.
pub enum Document {
    Dependent(Box<LoggedDependent>),
    Independent(Box<LoggedIndependent>),
    DependentLoro(Box<LoggedCollaborative>),
}

/// A snapshot of a single node's state, for the inspector panel.
pub struct NodeInfo {
    pub parents: Vec<u64>,
    pub children: Vec<u64>,
    pub active: bool,
    pub bookmarked: bool,
    pub content_len: usize,
}

impl Document {
    pub fn new_dependent(weave: DemoWeave) -> Self {
        Self::Dependent(Box::new(LoggedWeave::from(weave)))
    }

    pub fn new_independent(weave: IndependentDemoWeave) -> Self {
        Self::Independent(Box::new(LoggedWeave::from(weave)))
    }

    pub fn new_collaborative(weave: CollaborativeDemoWeave) -> Self {
        Self::DependentLoro(Box::new(LoggedWeave::from(weave)))
    }

    /// Converts a regular dependent weave into a collaborative document.
    pub fn collaborative_from_weave(weave: DemoWeave) -> Result<Self, String> {
        CollaborativeDemoWeave::from_weave(weave)
            .map(Self::new_collaborative)
            .map_err(|e| format!("could not create collaborative document: {e}"))
    }

    /// Creates a new document of the given kind with a single empty, active root node.
    pub fn empty(kind: WeaveKind) -> Self {
        let contents = TextContent::default();
        match kind {
            WeaveKind::Dependent => {
                let mut weave = DemoWeave::with_capacity(8, "Untitled document".to_string());
                weave.add_node(DemoNode {
                    id: 0,
                    from: None,
                    to: IndexSet::default(),
                    active: true,
                    bookmarked: false,
                    contents,
                });
                Self::new_dependent(weave)
            }
            WeaveKind::Independent => {
                let mut weave =
                    IndependentDemoWeave::with_capacity(8, "Untitled document".to_string());
                weave.add_node(IndependentDemoNode {
                    id: 0,
                    from: IndexSet::default(),
                    to: IndexSet::default(),
                    active: true,
                    bookmarked: false,
                    contents,
                });
                Self::new_independent(weave)
            }
            WeaveKind::DependentLoro => {
                let mut weave = DemoWeave::with_capacity(8, "Untitled document".to_string());
                weave.add_node(DemoNode {
                    id: 0,
                    from: None,
                    to: IndexSet::default(),
                    active: true,
                    bookmarked: false,
                    contents,
                });
                Self::collaborative_from_weave(weave)
                    .expect("an empty dependent weave always converts to Loro")
            }
        }
    }

    pub fn kind(&self) -> WeaveKind {
        match self {
            Self::Dependent(_) => WeaveKind::Dependent,
            Self::Independent(_) => WeaveKind::Independent,
            Self::DependentLoro(_) => WeaveKind::DependentLoro,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Dependent(weave) => weave.len(),
            Self::Independent(weave) => weave.len(),
            Self::DependentLoro(weave) => weave.len(),
        }
    }

    #[cfg(test)]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Dependent(weave) => weave.as_weave().validate(),
            Self::Independent(weave) => weave.as_weave().validate(),
            Self::DependentLoro(weave) => weave.as_weave().validate(),
        }
    }

    pub fn contains(&self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.contains(id),
            Self::Independent(weave) => weave.contains(id),
            Self::DependentLoro(weave) => weave.contains(id),
        }
    }

    pub fn max_id(&self) -> Option<u64> {
        match self {
            Self::Dependent(weave) => weave.nodes().keys().copied().max(),
            Self::Independent(weave) => weave.nodes().keys().copied().max(),
            Self::DependentLoro(weave) => weave.nodes().keys().copied().max(),
        }
    }

    pub fn metadata(&self) -> &String {
        match self {
            Self::Dependent(weave) => weave.metadata(),
            Self::Independent(weave) => weave.metadata(),
            Self::DependentLoro(weave) => weave.metadata(),
        }
    }

    pub fn set_metadata(&mut self, title: String) {
        match self {
            Self::Dependent(weave) => weave.metadata_mut(|metadata| *metadata = title),
            Self::Independent(weave) => weave.metadata_mut(|metadata| *metadata = title),
            Self::DependentLoro(weave) => weave.metadata_mut(|metadata| *metadata = title),
        }
    }

    pub fn bookmarks(&self) -> Vec<u64> {
        match self {
            Self::Dependent(weave) => weave.bookmarks().iter().copied().collect(),
            Self::Independent(weave) => weave.bookmarks().iter().copied().collect(),
            Self::DependentLoro(weave) => weave.bookmarks().iter().copied().collect(),
        }
    }

    pub fn node_info(&self, id: &u64) -> Option<NodeInfo> {
        match self {
            Self::Dependent(weave) => weave.get_node(id).map(|node| NodeInfo {
                parents: node.from.into_iter().collect(),
                children: node.to.iter().copied().collect(),
                active: node.active,
                bookmarked: node.bookmarked,
                content_len: node.contents.0.len(),
            }),
            Self::Independent(weave) => weave.get_node(id).map(|node| NodeInfo {
                parents: node.from.iter().copied().collect(),
                children: node.to.iter().copied().collect(),
                active: node.active,
                bookmarked: node.bookmarked,
                content_len: node.contents.0.len(),
            }),
            Self::DependentLoro(weave) => weave.get_node(id).map(|node| NodeInfo {
                parents: node.from.into_iter().collect(),
                children: node.to.iter().copied().collect(),
                active: node.active,
                bookmarked: node.bookmarked,
                content_len: node.contents.0.len(),
            }),
        }
    }

    pub fn node_contents(&self, id: &u64) -> Option<String> {
        match self {
            Self::Dependent(weave) => weave.get_node(id).map(|node| node.contents.0.clone()),
            Self::Independent(weave) => weave.get_node(id).map(|node| node.contents.0.clone()),
            Self::DependentLoro(weave) => weave.get_node(id).map(|node| node.contents.0.clone()),
        }
    }

    /// Builds the view-model for the tree view, in the weave's stable node ordering.
    pub fn tree_nodes(&mut self) -> Vec<TreeNode> {
        let mut order = Vec::new();
        match self {
            Self::Dependent(weave) => weave.get_ordered_node_identifiers(&mut order),
            Self::Independent(weave) => weave.get_ordered_node_identifiers(&mut order),
            Self::DependentLoro(weave) => weave.get_ordered_node_identifiers(&mut order),
        }

        let mut nodes = Vec::with_capacity(order.len());
        for id in order {
            match self {
                Self::Dependent(weave) => {
                    if let Some(node) = weave.get_node(&id) {
                        nodes.push(TreeNode {
                            id: node.id,
                            parents: node.from.into_iter().collect(),
                            contents: node.contents.0.clone(),
                            bookmarked: node.bookmarked,
                        });
                    }
                }
                Self::Independent(weave) => {
                    if let Some(node) = weave.get_node(&id) {
                        nodes.push(TreeNode {
                            id: node.id,
                            parents: node.from.iter().copied().collect(),
                            contents: node.contents.0.clone(),
                            bookmarked: node.bookmarked,
                        });
                    }
                }
                Self::DependentLoro(weave) => {
                    if let Some(node) = weave.get_node(&id) {
                        nodes.push(TreeNode {
                            id: node.id,
                            parents: node.from.into_iter().collect(),
                            contents: node.contents.0.clone(),
                            bookmarked: node.bookmarked,
                        });
                    }
                }
            }
        }

        nodes
    }

    /// The set of currently active nodes.
    ///
    /// For dependent documents this is at most one node (the cursor tip); for independent
    /// documents every node on the active path is active.
    pub fn active_set(&self) -> HashSet<u64> {
        match self {
            Self::Dependent(weave) => weave.active().into_iter().collect(),
            Self::Independent(weave) => weave.active().iter().copied().collect(),
            Self::DependentLoro(weave) => weave.active().into_iter().collect(),
        }
    }

    /// The active path, ordered from the tip node up to a root.
    pub fn active_path(&mut self) -> Vec<u64> {
        let mut path = Vec::new();
        match self {
            Self::Dependent(weave) => weave.get_active_path(&mut path),
            Self::Independent(weave) => weave.get_active_path(&mut path),
            Self::DependentLoro(weave) => weave.get_active_path(&mut path),
        }
        path
    }

    /// The tip of the active path, if any.
    pub fn active_tip(&mut self) -> Option<u64> {
        self.active_path().first().copied()
    }

    /// Adds a new active root node with the given id.
    pub fn add_root(&mut self, id: u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.add_node(DemoNode {
                id,
                from: None,
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::Independent(weave) => weave.add_node(IndependentDemoNode {
                id,
                from: IndexSet::default(),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::DependentLoro(weave) => weave.add_node(DemoNode {
                id,
                from: None,
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
        }
    }

    /// Adds a new active child node with the given id under a single parent.
    pub fn add_child(&mut self, parent: &u64, id: u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.add_node(DemoNode {
                id,
                from: Some(*parent),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::Independent(weave) => weave.add_node(IndependentDemoNode {
                id,
                from: IndexSet::from_iter([*parent]),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::DependentLoro(weave) => weave.add_node(DemoNode {
                id,
                from: Some(*parent),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
        }
    }

    /// Makes the given node active (the tip of the active path).
    pub fn set_active(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.set_node_active_status(id, true),
            Self::Independent(weave) => weave.set_node_active_status(id, true),
            Self::DependentLoro(weave) => weave.set_node_active_status(id, true),
        }
    }

    /// Makes the given node inactive.
    pub fn set_inactive(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.set_node_active_status(id, false),
            Self::Independent(weave) => weave.set_node_active_status(id, false),
            Self::DependentLoro(weave) => weave.set_node_active_status(id, false),
        }
    }

    /// Toggles the active status of a node.
    pub fn toggle_active(&mut self, id: &u64) -> bool {
        if self.node_info(id).is_some_and(|info| info.active) {
            self.set_inactive(id)
        } else {
            self.set_active(id)
        }
    }

    pub fn set_bookmarked(&mut self, id: &u64, value: bool) -> bool {
        match self {
            Self::Dependent(weave) => weave.set_node_bookmarked_status(id, value),
            Self::Independent(weave) => weave.set_node_bookmarked_status(id, value),
            Self::DependentLoro(weave) => weave.set_node_bookmarked_status(id, value),
        }
    }

    /// Replaces the contents of a node, returning `false` if the node does not exist.
    pub fn apply_edit(&mut self, id: &u64, text: String) -> bool {
        match self {
            Self::Dependent(weave) => weave
                .get_contents_mut(id, |contents| contents.0 = text)
                .is_some(),
            Self::Independent(weave) => weave
                .get_contents_mut(id, |contents| contents.0 = text)
                .is_some(),
            Self::DependentLoro(weave) => weave
                .get_contents_mut(id, |contents| contents.0 = text)
                .is_some(),
        }
    }

    /// Splits a node's contents at the given byte index; the tail becomes node `new_id`.
    pub fn split(&mut self, id: &u64, at: usize, new_id: u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.split_node(id, at, new_id),
            Self::Independent(weave) => weave.split_node(id, at, new_id),
            Self::DependentLoro(_) => false,
        }
    }

    /// Merges a node with its parent, returning the merged node's id on success.
    pub fn merge_with_parent(&mut self, id: &u64) -> Option<u64> {
        match self {
            Self::Dependent(weave) => weave.merge_with_parent(id),
            Self::Independent(weave) => weave.merge_with_parent(id),
            Self::DependentLoro(_) => None,
        }
    }

    /// Sorts a node's children by their contents.
    pub fn sort_children(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => {
                weave.sort_node_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
            Self::Independent(weave) => {
                weave.sort_node_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
            Self::DependentLoro(weave) => {
                weave.sort_node_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
        }
    }

    /// Sorts a node's children by their identifiers.
    pub fn sort_children_by_id(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.sort_node_children_by_id(id, Ord::cmp),
            Self::Independent(weave) => weave.sort_node_children_by_id(id, Ord::cmp),
            Self::DependentLoro(weave) => weave.sort_node_children_by_id(id, Ord::cmp),
        }
    }

    /// Removes a node (and any nodes orphaned by the removal), returning the number removed.
    pub fn remove(&mut self, id: &u64) -> Option<usize> {
        let mut removed = 0usize;
        let existed = match self {
            Self::Dependent(weave) => weave.remove_node_tracked(id, |_| removed += 1),
            Self::Independent(weave) => weave.remove_node_tracked(id, |_| removed += 1),
            Self::DependentLoro(weave) => weave.remove_node_tracked(id, |_| removed += 1),
        };
        existed.then_some(removed)
    }

    /// Moves a node to a new set of parents (independent documents only).
    pub fn move_node(&mut self, id: &u64, new_parents: &[u64]) -> Result<(), String> {
        match self {
            Self::Dependent(_) => {
                Err("Moving nodes is only supported by IndependentWeave documents".to_string())
            }
            Self::Independent(weave) => {
                if weave.move_node(id, new_parents) {
                    Ok(())
                } else {
                    Err(format!(
                        "Move of #{id} rejected (unknown parent, self-parenting, or cycle)"
                    ))
                }
            }
            Self::DependentLoro(_) => {
                Err("Moving nodes is only supported by IndependentWeave documents".to_string())
            }
        }
    }

    pub fn action_count(&self) -> usize {
        match self {
            Self::Dependent(weave) => weave.as_actions().len(),
            Self::Independent(weave) => weave.as_actions().len(),
            Self::DependentLoro(weave) => weave.as_actions().len(),
        }
    }

    pub fn clear_actions(&mut self) {
        match self {
            Self::Dependent(weave) => weave.clear_actions(),
            Self::Independent(weave) => weave.clear_actions(),
            Self::DependentLoro(weave) => weave.clear_actions(),
        }
    }

    /// Human-readable summaries of the most recent logged actions, newest first.
    pub fn formatted_actions(&self) -> Vec<String> {
        match self {
            Self::Dependent(weave) => weave
                .as_actions()
                .iter()
                .rev()
                .take(MAX_SHOWN_ACTIONS)
                .map(format_action)
                .collect(),
            Self::Independent(weave) => weave
                .as_actions()
                .iter()
                .rev()
                .take(MAX_SHOWN_ACTIONS)
                .map(format_action)
                .collect(),
            Self::DependentLoro(weave) => weave
                .as_actions()
                .iter()
                .rev()
                .take(MAX_SHOWN_ACTIONS)
                .map(format_action)
                .collect(),
        }
    }

    /// Forks a collaborative document into an independent Loro peer with an empty action log.
    pub fn fork_collaborative(&self) -> Result<Self, String> {
        match self {
            Self::DependentLoro(weave) => Ok(Self::new_collaborative(weave.as_weave().clone())),
            _ => Err("only collaborative documents can be forked".to_string()),
        }
    }

    /// Exports a full-history Loro snapshot for persistence.
    pub fn export_collaborative_snapshot(&self) -> Result<Vec<u8>, String> {
        match self {
            Self::DependentLoro(weave) => {
                let mut snapshot = weave.as_weave().clone();
                snapshot
                    .export(ExportMode::Snapshot)
                    .map_err(|e| format!("Loro snapshot export failed: {e}"))
            }
            _ => Err("only collaborative documents have a Loro snapshot".to_string()),
        }
    }
}

/// Indicates which peer incorporated remote changes during a synchronization pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyncOutcome {
    pub peer_a_changed: bool,
    pub peer_b_changed: bool,
}

/// Exchanges all updates missing from each collaborative peer and validates both results.
///
/// Version vectors are captured before either import so divergent offline updates flow in both
/// directions. Action logs are cleared only after both imports and validations succeed.
pub fn synchronize_pair(
    peer_a: &mut Document,
    peer_b: &mut Document,
) -> Result<SyncOutcome, String> {
    let (Document::DependentLoro(peer_a), Document::DependentLoro(peer_b)) = (peer_a, peer_b)
    else {
        return Err("both synchronization endpoints must be collaborative documents".to_string());
    };

    let a_before = peer_a.weave.oplog_vv();
    let b_before = peer_b.weave.oplog_vv();
    let updates_for_b = peer_a
        .weave
        .export(ExportMode::updates(&b_before))
        .map_err(|e| format!("Peer A export failed: {e}"))?;
    let updates_for_a = peer_b
        .weave
        .export(ExportMode::updates(&a_before))
        .map_err(|e| format!("Peer B export failed: {e}"))?;

    let a_import = peer_a
        .weave
        .update(|doc| doc.import(&updates_for_a))
        .map_err(|e| format!("Peer A validation/import failed: {e}"))?
        .map_err(|e| format!("Peer A import failed: {e}"))?;
    if a_import.pending.is_some() {
        return Err("Peer A import is missing dependent updates".to_string());
    }

    let b_import = peer_b
        .weave
        .update(|doc| doc.import(&updates_for_b))
        .map_err(|e| format!("Peer B validation/import failed: {e}"))?
        .map_err(|e| format!("Peer B import failed: {e}"))?;
    if b_import.pending.is_some() {
        return Err("Peer B import is missing dependent updates".to_string());
    }

    if !peer_a.weave.validate() {
        return Err("Peer A failed post-synchronization validation".to_string());
    }
    if !peer_b.weave.validate() {
        return Err("Peer B failed post-synchronization validation".to_string());
    }

    let outcome = SyncOutcome {
        peer_a_changed: peer_a.weave.oplog_vv() != a_before,
        peer_b_changed: peer_b.weave.oplog_vv() != b_before,
    };
    peer_a.clear_actions();
    peer_b.clear_actions();
    Ok(outcome)
}

/// Builds the sample dependent document shown on startup.
pub fn seeded_dependent() -> Document {
    let mut weave = DemoWeave::with_capacity(16, "The Lighthouse Letter".to_string());

    let node = |id: u64, from: Option<u64>, active: bool, bookmarked: bool, text: &str| DemoNode {
        id,
        from,
        to: IndexSet::default(),
        active,
        bookmarked,
        contents: TextContent(text.to_string()),
    };

    weave.add_node(node(
        0,
        None,
        false,
        false,
        "The lighthouse keeper found the letter on a Tuesday. ",
    ));
    weave.add_node(node(
        1,
        Some(0),
        false,
        true,
        "It was written in a language that smelled of salt. ",
    ));
    weave.add_node(node(
        3,
        Some(1),
        true,
        false,
        "She read it three times before the lamp went out. ",
    ));
    weave.add_node(node(
        2,
        Some(0),
        false,
        false,
        "It was addressed to someone who had drowned fifty years ago. ",
    ));
    weave.add_node(node(
        4,
        Some(2),
        false,
        false,
        "That night, the sea began to knock.",
    ));

    Document::new_dependent(weave)
}

/// Builds the seeded tree as a collaborative Loro document.
#[cfg(test)]
pub fn seeded_collaborative() -> Document {
    let Document::Dependent(logged) = seeded_dependent() else {
        unreachable!()
    };
    Document::collaborative_from_weave(logged.into_weave()).unwrap()
}

/// Builds a sample independent document, exercising the built-in
/// `From<DependentWeave>` conversion.
#[cfg(test)]
pub fn seeded_independent() -> Document {
    let Document::Dependent(logged) = seeded_dependent() else {
        unreachable!()
    };
    Document::new_independent(IndependentDemoWeave::from((*logged).into_weave()))
}

/// The node data needed to format a logged action, abstracted over both node types.
trait FormatNode: Node<u64, TextContent> {
    fn parent_ids(&self) -> Vec<u64>;
}

impl FormatNode for DemoNode {
    fn parent_ids(&self) -> Vec<u64> {
        self.from.into_iter().collect()
    }
}

impl FormatNode for IndependentDemoNode {
    fn parent_ids(&self) -> Vec<u64> {
        self.from.iter().copied().collect()
    }
}

/// Produces a short human-readable summary of a logged action.
fn format_action<N: FormatNode>(action: &WeaveAction<u64, N, TextContent, String>) -> String {
    match action {
        WeaveAction::AddNode(node) => {
            let parents = node.parent_ids();
            let parent = if parents.is_empty() {
                "root".to_string()
            } else {
                parents
                    .iter()
                    .map(|parent| format!("#{parent}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            format!("AddNode      #{id:<4} parent={parent}", id = node.id())
        }
        WeaveAction::SetNodeActiveStatus { id, value } => {
            format!("SetActive    #{id:<4} value={value}")
        }
        WeaveAction::SetNodeBookmarkedStatus { id, value } => {
            format!("SetBookmark  #{id:<4} value={value}")
        }
        WeaveAction::RemoveNode(id) => format!("RemoveNode   #{id}"),
        WeaveAction::RemoveAllNodes => "RemoveAll".to_string(),
        WeaveAction::SetMetadata(metadata) => format!("SetMetadata  \"{metadata}\""),
        WeaveAction::SetNodeChildOrdering { parent, children } => {
            let parent = parent.map_or_else(|| "roots".to_string(), |p| format!("#{p}"));
            format!("Reorder      {parent} → {children:?}")
        }
        WeaveAction::SetBookmarkOrdering(order) => format!("ReorderBookmarks {order:?}"),
        WeaveAction::SetActivePath(path) => format!("SetActivePath {path:?}"),
        WeaveAction::MoveNode { id, new_parents } => {
            format!("MoveNode     #{id} → {new_parents:?}")
        }
        WeaveAction::SetNodeContent { id, contents } => {
            format!("SetContent   #{id:<4} {} bytes", contents.0.len())
        }
        WeaveAction::SplitNode { id, at, new_id } => {
            format!("SplitNode    #{id:<4} at={at} → #{new_id}")
        }
        WeaveAction::MergeNodeWithParent(id) => format!("MergeNode    #{id}"),
        _ => "Other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_dependent_is_valid() {
        let mut document = seeded_dependent();

        assert_eq!(document.kind(), WeaveKind::Dependent);
        assert!(document.is_valid());
        assert_eq!(document.len(), 5);
        assert_eq!(document.active_tip(), Some(3));
        assert!(document.bookmarks().contains(&1));
    }

    #[test]
    fn seeded_independent_is_valid() {
        let mut document = seeded_independent();

        assert_eq!(document.kind(), WeaveKind::Independent);
        assert!(document.is_valid());
        assert_eq!(document.len(), 5);
        assert_eq!(document.active_tip(), Some(3));
        assert!(document.bookmarks().contains(&1));
        // The whole active path is flagged active in an independent weave.
        assert_eq!(document.active_set(), HashSet::from([0, 1, 3]));
    }

    #[test]
    fn empty_documents_are_valid() {
        for kind in [
            WeaveKind::Dependent,
            WeaveKind::Independent,
            WeaveKind::DependentLoro,
        ] {
            let mut document = Document::empty(kind);

            assert!(document.is_valid());
            assert_eq!(document.len(), 1);
            assert_eq!(document.active_tip(), Some(0));
        }
    }

    #[test]
    fn nodes_can_be_set_inactive() {
        for mut document in [seeded_dependent(), seeded_independent()] {
            assert!(document.node_info(&3).unwrap().active);

            assert!(document.set_inactive(&3));
            assert!(!document.node_info(&3).unwrap().active);
            assert!(document.is_valid());

            assert!(!document.set_inactive(&u64::MAX));
        }
    }

    #[test]
    fn nodes_can_toggle_active_status() {
        for mut document in [seeded_dependent(), seeded_independent()] {
            assert!(document.node_info(&3).unwrap().active);

            assert!(document.toggle_active(&3));
            assert!(!document.node_info(&3).unwrap().active);

            assert!(document.toggle_active(&3));
            assert!(document.node_info(&3).unwrap().active);
            assert!(document.is_valid());
        }
    }

    #[test]
    fn children_can_be_sorted_by_id() {
        for kind in [WeaveKind::Dependent, WeaveKind::Independent] {
            let mut document = Document::empty(kind);
            assert!(document.add_child(&0, 2));
            assert!(document.add_child(&0, 1));
            assert_eq!(document.node_info(&0).unwrap().children, vec![2, 1]);

            assert!(document.sort_children_by_id(&0));
            assert_eq!(document.node_info(&0).unwrap().children, vec![1, 2]);
            assert!(document.is_valid());
        }
    }

    #[test]
    fn move_node_reparents() {
        let mut weave = IndependentDemoWeave::with_capacity(8, String::new());
        let node = |id: u64, parents: &[u64]| IndependentDemoNode {
            id,
            from: parents.iter().copied().collect(),
            to: IndexSet::default(),
            active: false,
            bookmarked: false,
            contents: TextContent::default(),
        };

        weave.add_node(node(0, &[]));
        weave.add_node(node(1, &[0]));
        weave.add_node(node(2, &[0]));
        weave.add_node(node(3, &[1]));

        let mut document = Document::new_independent(weave);

        // Re-parent #3 from #1 to #2.
        document.move_node(&3, &[2]).unwrap();
        assert_eq!(document.node_info(&3).unwrap().parents, vec![2]);
        assert!(document.is_valid());

        // Moving under itself or under its own descendant is rejected.
        assert!(document.move_node(&3, &[3]).is_err());
        assert!(document.move_node(&0, &[3]).is_err());
        assert!(document.is_valid());

        // Moving to no parents turns the node into a root.
        document.move_node(&3, &[]).unwrap();
        assert_eq!(document.node_info(&3).unwrap().parents, Vec::<u64>::new());
        assert!(document.is_valid());

        // Dependent documents do not support moving.
        assert!(seeded_dependent().move_node(&3, &[2]).is_err());
    }

    #[test]
    fn independent_remove_keeps_shared_children() {
        let mut weave = IndependentDemoWeave::with_capacity(8, String::new());
        let node = |id: u64, parents: &[u64]| IndependentDemoNode {
            id,
            from: parents.iter().copied().collect(),
            to: IndexSet::default(),
            active: false,
            bookmarked: false,
            contents: TextContent::default(),
        };

        weave.add_node(node(0, &[]));
        weave.add_node(node(1, &[0]));
        weave.add_node(node(2, &[0]));
        weave.add_node(node(3, &[1, 2])); // shared child

        let mut document = Document::new_independent(weave);

        // Removing #1 keeps #3 alive via its other parent #2.
        assert_eq!(document.remove(&1), Some(1));
        assert!(document.contains(&3));
        assert_eq!(document.node_info(&3).unwrap().parents, vec![2]);
        assert!(document.is_valid());

        // Removing #0 cascades: #2 loses its only parent, then #3 loses its last one.
        assert_eq!(document.remove(&0), Some(3));
        assert_eq!(document.len(), 0);
        assert!(document.is_valid());
    }

    fn assert_collaborative_equal(a: &Document, b: &Document) {
        let (Document::DependentLoro(a), Document::DependentLoro(b)) = (a, b) else {
            panic!("expected collaborative documents");
        };
        assert_eq!(a.as_weave().as_weave(), b.as_weave().as_weave());
        assert!(a.as_weave().validate());
        assert!(b.as_weave().validate());
    }

    #[test]
    fn seeded_collaborative_is_valid_and_supports_tree_operations() {
        let mut document = seeded_collaborative();
        assert_eq!(document.kind(), WeaveKind::DependentLoro);
        assert!(document.is_valid());
        assert!(document.apply_edit(&3, "collaborative edit".to_string()));
        assert!(document.set_bookmarked(&3, true));
        assert!(document.add_child(&3, 5));
        assert!(document.set_active(&5));
        assert!(document.sort_children_by_id(&0));
        assert!(document.is_valid());
        assert!(!document.split(&3, 2, 6));
        assert_eq!(document.merge_with_parent(&3), None);
    }

    #[test]
    fn collaborative_one_way_sync_and_noop() {
        let mut a = seeded_collaborative();
        let mut b = a.fork_collaborative().unwrap();
        assert!(a.apply_edit(&3, "written by A".to_string()));

        let outcome = synchronize_pair(&mut a, &mut b).unwrap();
        assert!(!outcome.peer_a_changed);
        assert!(outcome.peer_b_changed);
        assert_collaborative_equal(&a, &b);
        assert_eq!(a.action_count(), 0);
        assert_eq!(b.action_count(), 0);

        let outcome = synchronize_pair(&mut a, &mut b).unwrap();
        assert_eq!(outcome, SyncOutcome::default());
        assert_collaborative_equal(&a, &b);
    }

    #[test]
    fn divergent_edits_converge_and_logs_clear_only_on_sync() {
        let mut a = seeded_collaborative();
        let mut b = a.fork_collaborative().unwrap();
        assert!(a.add_child(&3, 5));
        assert!(a.apply_edit(&3, "A's field value".to_string()));
        assert!(b.add_child(&3, 6));
        assert!(b.apply_edit(&3, "B's field value".to_string()));
        assert!(a.action_count() >= 2);
        assert!(b.action_count() >= 2);

        let outcome = synchronize_pair(&mut a, &mut b).unwrap();
        assert!(outcome.peer_a_changed);
        assert!(outcome.peer_b_changed);
        assert_collaborative_equal(&a, &b);
        assert!(a.contains(&5) && a.contains(&6));
        assert_eq!(a.node_contents(&3), b.node_contents(&3));
        assert_eq!(a.action_count(), 0);
        assert_eq!(b.action_count(), 0);
    }

    #[test]
    fn deletion_propagates() {
        let mut a = seeded_collaborative();
        let mut b = a.fork_collaborative().unwrap();
        assert_eq!(a.remove(&2), Some(2));
        synchronize_pair(&mut a, &mut b).unwrap();
        assert!(!b.contains(&2));
        assert!(!b.contains(&4));
        assert_collaborative_equal(&a, &b);
    }
}
