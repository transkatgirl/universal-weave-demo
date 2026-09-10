//! A document: either weave implementation plus its action log, behind a single API.

use std::collections::HashSet;
use std::iter;
use std::ops::Range;

use dissimilar::Chunk;
use unicode_segmentation::GraphemeCursor;
use universal_weave::indexmap::IndexSet;
use universal_weave::loro::ExportMode;
use universal_weave::wrappers::{LoggedWeave, PatchablePathWeave, WeaveAction};
use universal_weave::{
    ActivePathWeave, ActiveSingularWeave, BookmarkableWeave, DiscreteWeave, IndependentWeave,
    MetadataWeave, Node, SemiIndependentWeave, SortableWeave, Weave,
};

use crate::content::{
    CollaborativeDemoWeave, DemoNode, DemoWeave, IndependentDemoNode, IndependentDemoWeave,
    TextContent,
};
use crate::tree_view::{self, TreeLayout, TreeNode};

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
type PatchableIndependent =
    PatchablePathWeave<LoggedIndependent, u64, IndependentDemoNode, TextContent>;
type LoggedCollaborative = LoggedWeave<CollaborativeDemoWeave, u64, DemoNode, TextContent, String>;

/// A document wraps one weave implementation, plus its `LoggedWeave` action log.
///
/// Variants are boxed to keep the enum small regardless of implementation size.
pub enum Document {
    Dependent(Box<LoggedDependent>),
    Independent(Box<PatchableIndependent>),
    DependentLoro(Box<LoggedCollaborative>),
}

/// What an applied active-path edit changed, for status reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathEditSummary {
    /// The number of changed ranges applied as separate path patches.
    pub hunks: usize,
    /// The number of nodes created by splitting and inserting.
    pub created_nodes: usize,
    /// The number of changes satisfied by switching to a branch that already held the
    /// new text, instead of creating nodes.
    pub reused_branches: usize,
    /// The node on the new active path at the first change, for the inspector to select.
    pub focus: Option<u64>,
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
        Self::Independent(Box::new(PatchableIndependent::from(
            LoggedIndependent::from(weave),
        )))
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
                weave.insert(DemoNode {
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
                weave.insert(IndependentDemoNode {
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
                weave.insert(DemoNode {
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
            Self::Independent(weave) => weave.as_inner().as_weave().validate(),
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
            Self::Dependent(weave) => weave.get(id).map(|node| NodeInfo {
                parents: node.from.into_iter().collect(),
                children: node.to.iter().copied().collect(),
                active: node.active,
                bookmarked: node.bookmarked,
                content_len: node.contents.0.len(),
            }),
            Self::Independent(weave) => weave.get(id).map(|node| NodeInfo {
                parents: node.from.iter().copied().collect(),
                children: node.to.iter().copied().collect(),
                active: node.active,
                bookmarked: node.bookmarked,
                content_len: node.contents.0.len(),
            }),
            Self::DependentLoro(weave) => weave.get(id).map(|node| NodeInfo {
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
            Self::Dependent(weave) => weave.get(id).map(|node| node.contents.0.clone()),
            Self::Independent(weave) => weave.get(id).map(|node| node.contents.0.clone()),
            Self::DependentLoro(weave) => weave.get(id).map(|node| node.contents.0.clone()),
        }
    }

    /// Builds the view-model for the tree view, in the weave's stable node ordering.
    pub fn tree_nodes(&mut self) -> Vec<TreeNode> {
        let mut order = Vec::new();
        match self {
            Self::Dependent(weave) => weave.get_ordered_identifiers(&mut order),
            Self::Independent(weave) => weave.get_ordered_identifiers(&mut order),
            Self::DependentLoro(weave) => weave.get_ordered_identifiers(&mut order),
        }

        let mut nodes = Vec::with_capacity(order.len());
        for id in order {
            match self {
                Self::Dependent(weave) => {
                    if let Some(node) = weave.get(&id) {
                        nodes.push(TreeNode {
                            id: node.id,
                            parents: node.from.into_iter().collect(),
                            contents: node.contents.0.clone(),
                            bookmarked: node.bookmarked,
                        });
                    }
                }
                Self::Independent(weave) => {
                    if let Some(node) = weave.get(&id) {
                        nodes.push(TreeNode {
                            id: node.id,
                            parents: node.from.iter().copied().collect(),
                            contents: node.contents.0.clone(),
                            bookmarked: node.bookmarked,
                        });
                    }
                }
                Self::DependentLoro(weave) => {
                    if let Some(node) = weave.get(&id) {
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

    /// Computes the geometry for the 2D view directly from the underlying
    /// weave, preserving each implementation's topological ordering.
    pub fn tree_layout(&mut self) -> TreeLayout {
        match self {
            Self::Dependent(weave) => tree_view::layout::<_, DemoNode, TextContent>(weave.as_mut()),
            Self::Independent(weave) => {
                tree_view::layout::<_, IndependentDemoNode, TextContent>(weave.as_mut())
            }
            Self::DependentLoro(weave) => {
                tree_view::layout::<_, DemoNode, TextContent>(weave.as_mut())
            }
        }
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

    /// Returns the active-path identifiers (tip to root) and their concatenated text
    /// (root to tip). The paired snapshot can be used to detect staged edits that
    /// became stale after a structural operation.
    pub fn active_path_text(&mut self) -> (Vec<u64>, String) {
        let path = self.active_path();
        let text = path
            .iter()
            .rev()
            .filter_map(|id| self.node_contents(id))
            .collect();
        (path, text)
    }

    /// Applies the changes between `expected_text` and `edited_text` to an independent
    /// document's active path.
    ///
    /// Each changed range found by [`text_hunks`] becomes one branch-preserving path
    /// patch: a single [`PatchablePathWeave::replace`] call, or a
    /// [`PatchablePathWeave::split_out`] call when the hunk only deletes text. Hunks
    /// are applied from the end of the text backwards so earlier byte offsets stay
    /// valid and unchanged text between changes stays shared. Removed text stays
    /// reachable on alternate branches, and inserted text continues only into the
    /// active path, never into the text it replaced.
    ///
    /// Text appended to a path whose tip is an empty node without continuations, such
    /// as a node just added from the inspector, fills that placeholder instead of
    /// inserting in front of it and leaving it dangling at the end of the path.
    ///
    /// A change whose new text already exists as a branch at that position, for example
    /// a word changed back to what it was before an earlier apply, switches the active
    /// path to that branch (see [`reuse_branch`]) instead of adding a duplicate node.
    ///
    /// `expected_path` and `expected_text` must still match the document. Generated
    /// nodes consume identifiers from the editor's existing arithmetic sequence.
    /// The operation is prepared on a clone so identifier errors cannot leave a
    /// partially applied structural patch behind.
    ///
    /// Returns `None` when the texts are identical and nothing was applied.
    pub fn replace_active_path_text(
        &mut self,
        expected_path: &[u64],
        expected_text: &str,
        edited_text: &str,
        next_id: &mut u64,
        id_step: u64,
    ) -> Result<Option<PathEditSummary>, String> {
        let (current_path, current_text) = self.active_path_text();
        if current_path != expected_path || current_text != expected_text {
            return Err(
                "Active path changed while this edit was staged; restore the path or reset the buffer before applying"
                    .to_string(),
            );
        }
        if expected_text == edited_text {
            return Ok(None);
        }

        let Self::Independent(weave) = self else {
            return Err(
                "Active-path editing is only available for independent documents".to_string(),
            );
        };

        // An empty, childless tip is a placeholder for text about to be typed, so an
        // append fills it rather than inserting in front of it. A placeholder with
        // several parents is left alone, since filling it would change every path
        // through it.
        let placeholder_tip = current_path.first().copied().filter(|tip| {
            weave.get(tip).is_some_and(|node| {
                node.contents.0.is_empty() && node.to.is_empty() && node.from.len() <= 1
            })
        });

        let hunks = text_hunks(expected_text, edited_text);
        let hunk_count = hunks.len();
        let first_change = hunks.first().map(|hunk| hunk.new.start);
        let apply = |target: &mut PatchableIndependent, generate_id: &mut dyn FnMut() -> u64| {
            let mut reused = 0_usize;
            // Later hunks first, so the byte offsets of earlier hunks stay valid.
            for hunk in hunks.iter().rev() {
                let replacement = &edited_text[hunk.new.clone()];
                if let Some(tip) = placeholder_tip
                    && hunk.old.is_empty()
                    && hunk.old.start == expected_text.len()
                {
                    let filled = target
                        .get_contents_mut(&tip, |contents| contents.0 = replacement.to_owned());
                    debug_assert!(
                        filled.is_some(),
                        "the placeholder tip is on the active path"
                    );
                    continue;
                }
                if replacement.is_empty() {
                    // A pure deletion only detaches the old range; `replace` would
                    // leave an empty node behind on the active path.
                    target.split_out(hunk.old.clone(), &mut *generate_id);
                } else if reuse_branch(target, hunk.old.clone(), replacement) {
                    // The new text already exists as a branch here, typically because
                    // it is being changed back to what an earlier apply replaced.
                    reused += 1;
                } else {
                    // `prefix_all` stays false: the inserted node continues only into the
                    // active path, so it is never linked to the text it replaced and
                    // repeated insertions at one position grow the graph linearly.
                    target.replace(
                        hunk.old.clone(),
                        TextContent(replacement.to_owned()),
                        false,
                        &mut *generate_id,
                    );
                }
            }
            reused
        };

        // The wrapper's id callback is infallible, so measure the exact demand on a
        // throwaway clone (fed any unused ids) before touching the editor's sequence.
        let mut demanded = 0_usize;
        let mut unused = (0_u64..).filter(|id| !weave.contains(id));
        let rehearsed_reuse = apply(&mut (**weave).clone(), &mut || {
            demanded += 1;
            unused.next().expect("an unused identifier always exists")
        });

        let mut ids = Vec::with_capacity(demanded);
        let mut sequence = iter::successors(Some(*next_id), |id| id.checked_add(id_step));
        for _ in 0..demanded {
            let id = sequence.next().ok_or_else(|| {
                "Identifier sequence exhausted while preparing the path edit".to_string()
            })?;
            if weave.contains(&id) || ids.contains(&id) {
                return Err(format!(
                    "Identifier sequence is exhausted or collides at #{id}"
                ));
            }
            ids.push(id);
        }

        let mut patched = (**weave).clone();
        let mut remaining = ids.iter().copied();
        let reused_branches = apply(&mut patched, &mut || {
            remaining
                .next()
                .expect("identifier demand was measured in advance")
        });
        debug_assert!(remaining.next().is_none());
        debug_assert_eq!(reused_branches, rehearsed_reuse);

        if let Some(last) = ids.last() {
            *next_id = last.saturating_add(id_step);
        }
        **weave = patched;
        let focus = first_change.and_then(|offset| self.node_at_offset(offset));
        Ok(Some(PathEditSummary {
            hunks: hunk_count,
            created_nodes: demanded,
            reused_branches,
            focus,
        }))
    }

    /// The node on the active path whose text contains byte `offset`, or the last
    /// non-empty node when the offset is at the end of the path.
    fn node_at_offset(&mut self, offset: usize) -> Option<u64> {
        let path = self.active_path();
        let mut cursor = 0_usize;
        let mut last = None;
        for id in path.iter().rev() {
            let length = self.node_contents(id).map_or(0, |text| text.len());
            if length == 0 {
                continue;
            }
            if offset < cursor + length {
                return Some(*id);
            }
            cursor += length;
            last = Some(*id);
        }
        last.or_else(|| path.first().copied())
    }

    /// The tip of the active path, if any.
    pub fn active_tip(&mut self) -> Option<u64> {
        self.active_path().first().copied()
    }

    /// Whether `path`, ordered tip to root as [`Self::active_path`] returns it, still
    /// names a chain of existing nodes that ends at a root, so it can be made active
    /// again. An empty path can always be restored by deactivating every node.
    pub fn can_restore_path(&self, path: &[u64]) -> bool {
        path.last().is_none_or(|root| {
            self.node_info(root)
                .is_some_and(|info| info.parents.is_empty())
        }) && path.windows(2).all(|pair| {
            self.node_info(&pair[0])
                .is_some_and(|info| info.parents.contains(&pair[1]))
        })
    }

    /// Makes `path` (tip to root) the active path again, for example to resume a staged
    /// edit after a structural operation moved the active path elsewhere.
    ///
    /// Tree documents derive the path from its tip, so activating the tip restores it.
    pub fn restore_path(&mut self, path: &[u64]) -> Result<(), String> {
        if !self.can_restore_path(path) {
            return Err(
                "The previous path can no longer be restored; some of its nodes were removed or moved"
                    .to_string(),
            );
        }
        if let Self::Independent(weave) = self {
            weave.set_active_path(path.iter().copied());
            return Ok(());
        }
        match path.first() {
            Some(tip) => {
                self.set_active(tip);
            }
            None => {
                if let Some(tip) = self.active_tip() {
                    self.set_inactive(&tip);
                }
            }
        }
        Ok(())
    }

    /// Adds a new active root node with the given id.
    pub fn add_root(&mut self, id: u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.insert(DemoNode {
                id,
                from: None,
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::Independent(weave) => weave.insert(IndependentDemoNode {
                id,
                from: IndexSet::default(),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::DependentLoro(weave) => weave.insert(DemoNode {
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
            Self::Dependent(weave) => weave.insert(DemoNode {
                id,
                from: Some(*parent),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::Independent(weave) => weave.insert(IndependentDemoNode {
                id,
                from: IndexSet::from_iter([*parent]),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent::default(),
            }),
            Self::DependentLoro(weave) => weave.insert(DemoNode {
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
            Self::Dependent(weave) => weave.set_active(id, true),
            Self::Independent(weave) => weave.set_active(id, true),
            Self::DependentLoro(weave) => weave.set_active(id, true),
        }
    }

    /// Makes the given node inactive.
    pub fn set_inactive(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.set_active(id, false),
            Self::Independent(weave) => weave.set_active(id, false),
            Self::DependentLoro(weave) => weave.set_active(id, false),
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
            Self::Dependent(weave) => weave.set_bookmarked(id, value),
            Self::Independent(weave) => weave.set_bookmarked(id, value),
            Self::DependentLoro(weave) => weave.set_bookmarked(id, value),
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
            Self::Dependent(weave) => weave.split(id, at, new_id),
            Self::Independent(weave) => weave.split(id, at, new_id),
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
                weave.sort_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
            Self::Independent(weave) => {
                weave.sort_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
            Self::DependentLoro(weave) => {
                weave.sort_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
        }
    }

    /// Sorts a node's children by their identifiers.
    pub fn sort_children_by_id(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.sort_children_by_id(id, Ord::cmp),
            Self::Independent(weave) => weave.sort_children_by_id(id, Ord::cmp),
            Self::DependentLoro(weave) => weave.sort_children_by_id(id, Ord::cmp),
        }
    }

    /// Removes a node (and any nodes orphaned by the removal), returning the number removed.
    pub fn remove(&mut self, id: &u64) -> Option<usize> {
        let mut removed = 0usize;
        let existed = match self {
            Self::Dependent(weave) => weave.remove_tracked(id, |_| removed += 1),
            Self::Independent(weave) => weave.remove_tracked(id, |_| removed += 1),
            Self::DependentLoro(weave) => weave.remove_tracked(id, |_| removed += 1),
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
                if weave.move_to(id, new_parents) {
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
            Self::Independent(weave) => weave.as_inner().as_actions().len(),
            Self::DependentLoro(weave) => weave.as_actions().len(),
        }
    }

    pub fn clear_actions(&mut self) {
        match self {
            Self::Dependent(weave) => weave.clear_actions(),
            Self::Independent(weave) => weave.weave.clear_actions(),
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
                .as_inner()
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

/// The active path from root to tip, and the byte offset at which each node's text
/// starts; the final offset is the length of the whole path text.
fn path_layout(weave: &mut PatchableIndependent) -> (Vec<u64>, Vec<usize>) {
    let mut path = Vec::new();
    weave.get_active_path(&mut path);
    path.reverse();
    let mut starts = Vec::with_capacity(path.len() + 1);
    let mut cursor = 0_usize;
    for id in &path {
        starts.push(cursor);
        cursor += weave
            .get_contents(id)
            .map_or(0, |contents| contents.0.len());
    }
    starts.push(cursor);
    (path, starts)
}

/// Switches the active path to an existing branch that already holds `replacement` in
/// place of the `old` byte range, returning whether one was found.
///
/// [`PatchablePathWeave::replace`] would link a new node from the node before the range
/// (or make it a root) to the node after it (or make it the tip). A branch qualifies
/// when the range covers whole nodes and a chain of existing nodes runs between those
/// same neighbours with exactly the replacement text, so switching to it changes the
/// path text identically without adding a node. Zero-width nodes let several node
/// boundaries share one offset, so every combination that covers the range is tried.
fn reuse_branch(weave: &mut PatchableIndependent, old: Range<usize>, replacement: &str) -> bool {
    let (path, starts) = path_layout(weave);
    for i in (0..starts.len()).filter(|&i| starts[i] == old.start) {
        for j in (i..starts.len()).filter(|&j| starts[j] == old.end) {
            let parent = i.checked_sub(1).map(|index| path[index]);
            let child = path.get(j).copied();
            let heads: Vec<u64> = match parent {
                Some(parent) => weave
                    .get_children(&parent)
                    .map(|children| children.iter().copied().collect())
                    .unwrap_or_default(),
                None => weave.roots().iter().copied().collect(),
            };
            let mut branch = Vec::new();
            let mut visited = HashSet::new();
            if heads.into_iter().any(|head| {
                extend_branch(weave, head, replacement, child, &mut branch, &mut visited)
            }) {
                let active: Vec<u64> = path[..i]
                    .iter()
                    .chain(&branch)
                    .chain(&path[j..])
                    .copied()
                    .collect();
                weave.set_active_path(active.into_iter());
                return true;
            }
        }
    }
    false
}

/// Extends `branch` with `id` and then, depth first, with a chain of its descendants
/// whose contents spell out `remaining` and whose last node is a parent of `child`
/// (or any node, when there is no `child`). Leaves `branch` unchanged on failure.
///
/// `visited` records (node, bytes left) pairs that already failed, so shared
/// descendants are not searched again from the same position.
fn extend_branch(
    weave: &PatchableIndependent,
    id: u64,
    remaining: &str,
    child: Option<u64>,
    branch: &mut Vec<u64>,
    visited: &mut HashSet<(u64, usize)>,
) -> bool {
    let Some(rest) = weave
        .get_contents(&id)
        .and_then(|contents| remaining.strip_prefix(contents.0.as_str()))
    else {
        return false;
    };
    if !visited.insert((id, rest.len())) {
        return false;
    }
    let Some(children) = weave.get_children(&id) else {
        return false;
    };
    branch.push(id);
    if rest.is_empty() && child.is_none_or(|child| children.contains(&child)) {
        return true;
    }
    if children
        .iter()
        .any(|&next| extend_branch(weave, next, rest, child, branch, visited))
    {
        return true;
    }
    branch.pop();
    false
}

/// One changed range of a text edit, as byte offsets into the original and edited texts.
///
/// `old` is the replaced range of the original text and `new` the range of the edited
/// text that takes its place; either may be empty for a pure insertion or deletion.
/// All four boundaries are character boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TextHunk {
    old: Range<usize>,
    new: Range<usize>,
}

/// Finds every changed range between two UTF-8 strings, in text order.
///
/// Unchanged text between changes is left in place rather than folded into one
/// large replacement. Where a change could sit at several equivalent positions,
/// such as an inserted repeated word, it is aligned to the nearest word, sentence,
/// or line boundary. A change that starts or ends inside a grapheme cluster or a
/// word is then widened to the whole cluster or word, and changes that meet are
/// merged, so the old and new branches hold complete words rather than fragments
/// such as `ue` and `hur`, and a letter is never separated from its combining marks.
/// Punctuation is a boundary unless it joins two word characters, so `Tuesday.`
/// becoming `Thursday.` replaces only the word, while `don't` is widened as one word.
fn text_hunks(original: &str, edited: &str) -> Vec<TextHunk> {
    let mut hunks = Vec::new();
    let mut pending: Option<TextHunk> = None;
    let (mut old_cursor, mut new_cursor) = (0_usize, 0_usize);

    for chunk in dissimilar::diff(original, edited) {
        match chunk {
            Chunk::Equal(text) => {
                hunks.extend(pending.take());
                old_cursor += text.len();
                new_cursor += text.len();
            }
            Chunk::Delete(text) => {
                let hunk = pending.get_or_insert(TextHunk {
                    old: old_cursor..old_cursor,
                    new: new_cursor..new_cursor,
                });
                old_cursor += text.len();
                hunk.old.end = old_cursor;
            }
            Chunk::Insert(text) => {
                let hunk = pending.get_or_insert(TextHunk {
                    old: old_cursor..old_cursor,
                    new: new_cursor..new_cursor,
                });
                new_cursor += text.len();
                hunk.new.end = new_cursor;
            }
        }
    }
    hunks.extend(pending);

    debug_assert_eq!(old_cursor, original.len());
    debug_assert_eq!(new_cursor, edited.len());
    widen_to_words(original, edited, &hunks)
}

/// The grapheme cluster starting at byte `at` of `text`, if any.
///
/// `at` must be a cluster boundary.
fn grapheme_at(text: &str, at: usize) -> Option<&str> {
    let end = GraphemeCursor::new(at, text.len(), true)
        .next_boundary(text, 0)
        .ok()
        .flatten()?;
    Some(&text[at..end])
}

/// The grapheme cluster ending at byte `at` of `text`, if any.
///
/// `at` must be a cluster boundary.
fn grapheme_before(text: &str, at: usize) -> Option<&str> {
    let start = GraphemeCursor::new(at, text.len(), true)
        .prev_boundary(text, 0)
        .ok()
        .flatten()?;
    Some(&text[start..at])
}

/// Whether byte `at` of `text` sits between two grapheme clusters rather than inside one.
fn is_grapheme_boundary(text: &str, at: usize) -> bool {
    GraphemeCursor::new(at, text.len(), true)
        .is_boundary(text, 0)
        .unwrap_or(true)
}

/// Whether a grapheme cluster is a line break, which is laid out as a row break rather
/// than a glyph and so cannot carry a visible mark.
fn is_line_break(grapheme: &str) -> bool {
    matches!(grapheme, "\n" | "\r\n" | "\r")
}

/// The grapheme cluster of `text` to mark for text removed at byte `at`, if any.
///
/// The removal has no text of its own, so the cluster after it is marked, or the one
/// before it at the end of the text. A line break there would leave the mark
/// invisible, so the other neighbour is used instead: removing the end of a line marks
/// the line's last character. When line breaks surround the removal, as after removing
/// a whole line, the closest visible cluster before it is marked, so the mark ends the
/// line the removed text followed; at the start of the text, the closest one after it.
fn removal_mark(text: &str, at: usize) -> Option<Range<usize>> {
    let after = grapheme_at(text, at).map(|grapheme| at..at + grapheme.len());
    let before = grapheme_before(text, at).map(|grapheme| at - grapheme.len()..at);
    if let Some(mark) = [after, before]
        .into_iter()
        .flatten()
        .find(|mark| !is_line_break(&text[mark.clone()]))
    {
        return Some(mark);
    }
    let mut cursor = at;
    while let Some(grapheme) = grapheme_before(text, cursor) {
        cursor -= grapheme.len();
        if !is_line_break(grapheme) {
            return Some(cursor..cursor + grapheme.len());
        }
    }
    let mut cursor = at;
    while let Some(grapheme) = grapheme_at(text, cursor) {
        if !is_line_break(grapheme) {
            return Some(cursor..cursor + grapheme.len());
        }
        cursor += grapheme.len();
    }
    None
}

/// Whether a grapheme cluster carries a letter or digit, such as `e` with a combining accent.
fn is_alphanumeric_grapheme(grapheme: &str) -> bool {
    grapheme.chars().any(char::is_alphanumeric)
}

/// Whether the grapheme cluster starting at byte `at` of `text` belongs to a word.
///
/// Clusters carrying a letter or digit always do, including a base character with its
/// combining marks. Any other non-whitespace cluster does only when it joins two such
/// clusters, such as the apostrophe in `don't`, the hyphen in `well-known`, or the
/// point in `3.14`. Punctuation at the edge of a word or next to whitespace is a
/// boundary.
fn is_word_at(text: &str, at: usize) -> bool {
    let Some(grapheme) = grapheme_at(text, at) else {
        return false;
    };
    if is_alphanumeric_grapheme(grapheme) {
        return true;
    }
    if grapheme.chars().all(char::is_whitespace) {
        return false;
    }
    grapheme_before(text, at).is_some_and(is_alphanumeric_grapheme)
        && grapheme_at(text, at + grapheme.len()).is_some_and(is_alphanumeric_grapheme)
}

/// Whether the grapheme cluster ending at byte `at` of `text` belongs to a word.
fn is_word_ending_at(text: &str, at: usize) -> bool {
    grapheme_before(text, at).is_some_and(|grapheme| is_word_at(text, at - grapheme.len()))
}

/// Moves hunk boundaries that fall inside a grapheme cluster or a word out to the
/// surrounding cluster and word boundaries, then merges hunks that touch or overlap.
///
/// The unchanged text around a hunk is identical in both strings, so the text on the
/// shared side of a boundary is the same in `original` and `edited` and a boundary can
/// move over it in both. Only that text's neighbours, and so its cluster and
/// punctuation context, can differ, which is why each check consults both texts. A
/// boundary is inside a cluster when it separates a base character from its combining
/// marks or modifiers in either text; it is inside a word when a word cluster sits on
/// one side of it and another follows on the other side in either the old or the new
/// text.
fn widen_to_words(original: &str, edited: &str, hunks: &[TextHunk]) -> Vec<TextHunk> {
    let shared_boundary = |old_at: usize, new_at: usize| {
        is_grapheme_boundary(original, old_at) && is_grapheme_boundary(edited, new_at)
    };
    let shared_word_before = |old_at: usize, new_at: usize| {
        is_word_ending_at(original, old_at) || is_word_ending_at(edited, new_at)
    };
    let shared_word_after =
        |old_at: usize, new_at: usize| is_word_at(original, old_at) || is_word_at(edited, new_at);

    let mut widened: Vec<TextHunk> = Vec::with_capacity(hunks.len());
    for (index, hunk) in hunks.iter().enumerate() {
        let mut hunk = hunk.clone();
        let floor = index
            .checked_sub(1)
            .map_or(0, |previous| hunks[previous].old.end);
        let ceiling = hunks
            .get(index + 1)
            .map_or(original.len(), |next| next.old.start);

        // Step over single characters until both texts agree the edge is a cluster
        // boundary, so a node is never split between a base character and its marks.
        while hunk.old.start > floor
            && !shared_boundary(hunk.old.start, hunk.new.start)
            && let Some(character) = original[..hunk.old.start].chars().next_back()
        {
            hunk.old.start -= character.len_utf8();
            hunk.new.start -= character.len_utf8();
        }
        while hunk.old.end < ceiling
            && !shared_boundary(hunk.old.end, hunk.new.end)
            && let Some(character) = original[hunk.old.end..].chars().next()
        {
            hunk.old.end += character.len_utf8();
            hunk.new.end += character.len_utf8();
        }

        // Then step over whole clusters out to the edges of the surrounding words.
        if shared_word_before(hunk.old.start, hunk.new.start)
            && (is_word_at(original, hunk.old.start) || is_word_at(edited, hunk.new.start))
        {
            while hunk.old.start > floor
                && let Some(grapheme) = grapheme_before(original, hunk.old.start)
                && shared_word_before(hunk.old.start, hunk.new.start)
            {
                hunk.old.start -= grapheme.len();
                hunk.new.start -= grapheme.len();
            }
        }
        if shared_word_after(hunk.old.end, hunk.new.end)
            && (is_word_ending_at(original, hunk.old.end)
                || is_word_ending_at(edited, hunk.new.end))
        {
            while hunk.old.end < ceiling
                && let Some(grapheme) = grapheme_at(original, hunk.old.end)
                && shared_word_after(hunk.old.end, hunk.new.end)
            {
                hunk.old.end += grapheme.len();
                hunk.new.end += grapheme.len();
            }
        }

        match widened.last_mut() {
            Some(previous) if hunk.old.start <= previous.old.end => {
                previous.old.end = previous.old.end.max(hunk.old.end);
                previous.new.end = previous.new.end.max(hunk.new.end);
            }
            _ => widened.push(hunk),
        }
    }
    widened
}

/// How a span of an edited active-path text relates to the document, for previewing
/// an edit before it is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    /// Text the document already holds, in the node at this position on the path,
    /// counted from the root.
    Shared { node: usize },
    /// Text a change would add: it becomes a new node, or the path switches to an
    /// existing branch that already holds it.
    Changed,
}

/// One span of a previewed edit, as byte offsets into the edited text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSpan {
    pub range: Range<usize>,
    pub kind: PreviewKind,
    /// Whether this span is the visible character nearest to a point where text was
    /// removed without replacement, so the removal can be marked although it has no
    /// text of its own. See [`removal_mark`].
    pub marks_removal: bool,
}

/// What applying an edit would change, laid over the edited text for highlighting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditPreview {
    /// Non-empty spans covering the whole edited text in order, without gaps; adjacent
    /// spans always differ in kind or mark.
    pub spans: Vec<PreviewSpan>,
    /// The number of changes an apply would make; zero when the texts are identical.
    pub changes: usize,
}

/// Previews the changes between `original` and `edited` as
/// [`Document::replace_active_path_text`] would apply them.
///
/// `node_lengths` holds the byte length of each node's text along the path from root
/// to tip, so unchanged text can be attributed to its node. When the lengths do not
/// add up to `original`, for example because a node on the path was removed, the
/// whole text counts as one node.
pub fn preview_edit(original: &str, edited: &str, node_lengths: &[usize]) -> EditPreview {
    let hunks = text_hunks(original, edited);

    // Where each node's text starts in the original.
    let mut starts = Vec::with_capacity(node_lengths.len());
    let mut cursor = 0_usize;
    for length in node_lengths {
        starts.push(cursor);
        cursor += length;
    }
    if starts.is_empty() || cursor != original.len() {
        starts = vec![0];
    }

    // Span edges in the edited text: the edges of every change, every node boundary
    // no change swallowed (moved by the size difference of the changes before it),
    // and the edges of each character that marks a removal.
    let mut cuts = vec![0, edited.len()];
    let mut earlier = hunks.iter().peekable();
    let (mut added, mut removed) = (0_usize, 0_usize);
    for &start in &starts[1..] {
        while let Some(hunk) = earlier.next_if(|hunk| hunk.old.end <= start) {
            added += hunk.new.len();
            removed += hunk.old.len();
        }
        if earlier.peek().is_none_or(|hunk| hunk.old.start >= start) {
            cuts.push(start - removed + added);
        }
    }
    let mut removal_marks = Vec::new();
    for hunk in &hunks {
        cuts.push(hunk.new.start);
        cuts.push(hunk.new.end);
        if hunk.new.is_empty()
            && let Some(mark) = removal_mark(edited, hunk.new.start)
        {
            cuts.push(mark.start);
            cuts.push(mark.end);
            removal_marks.push(mark);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans: Vec<PreviewSpan> = Vec::with_capacity(cuts.len());
    let mut earlier = hunks.iter().peekable();
    let (mut added, mut removed) = (0_usize, 0_usize);
    for pair in cuts.windows(2) {
        let range = pair[0]..pair[1];
        while let Some(hunk) = earlier.next_if(|hunk| hunk.new.end <= range.start) {
            added += hunk.new.len();
            removed += hunk.old.len();
        }
        let kind = if earlier
            .peek()
            .is_some_and(|hunk| hunk.new.start <= range.start)
        {
            PreviewKind::Changed
        } else {
            let offset = range.start - added + removed;
            PreviewKind::Shared {
                node: starts.partition_point(|&start| start <= offset) - 1,
            }
        };
        let marks_removal = removal_marks
            .iter()
            .any(|mark| mark.start <= range.start && range.end <= mark.end);
        // A removal point splits otherwise identical text; keep such runs whole.
        match spans.last_mut() {
            Some(previous)
                if previous.kind == kind
                    && previous.marks_removal == marks_removal
                    && previous.range.end == range.start =>
            {
                previous.range.end = range.end;
            }
            _ => spans.push(PreviewSpan {
                range,
                kind,
                marks_removal,
            }),
        }
    }

    EditPreview {
        spans,
        changes: hunks.len(),
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

    weave.insert(node(
        0,
        None,
        false,
        false,
        "The lighthouse keeper found the letter on a Tuesday. ",
    ));
    weave.insert(node(
        1,
        Some(0),
        false,
        true,
        "It was written in a language that smelled of salt. ",
    ));
    weave.insert(node(
        3,
        Some(1),
        true,
        false,
        "She read it three times before the lamp went out. ",
    ));
    weave.insert(node(
        2,
        Some(0),
        false,
        false,
        "It was addressed to someone who had drowned fifty years ago. ",
    ));
    weave.insert(node(
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
        WeaveAction::Insert(node) => {
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
        WeaveAction::SetActive { id, value } => {
            format!("SetActive    #{id:<4} value={value}")
        }
        WeaveAction::SetBookmarked { id, value } => {
            format!("SetBookmark  #{id:<4} value={value}")
        }
        WeaveAction::Remove(id) => format!("RemoveNode   #{id}"),
        WeaveAction::Clear => "RemoveAll".to_string(),
        WeaveAction::SetMetadata(metadata) => format!("SetMetadata  \"{metadata}\""),
        WeaveAction::SetChildOrdering { parent, children } => {
            let parent = parent.map_or_else(|| "roots".to_string(), |p| format!("#{p}"));
            format!("Reorder      {parent} → {children:?}")
        }
        WeaveAction::SetBookmarkOrdering(order) => format!("ReorderBookmarks {order:?}"),
        WeaveAction::SetActivePath(path) => format!("SetActivePath {path:?}"),
        WeaveAction::MoveTo { id, new_parents } => {
            format!("MoveNode     #{id} → {new_parents:?}")
        }
        WeaveAction::SetContents { id, contents } => {
            format!("SetContent   #{id:<4} {} bytes", contents.0.len())
        }
        WeaveAction::Split { id, at, new_id } => {
            format!("SplitNode    #{id:<4} at={at} → #{new_id}")
        }
        WeaveAction::MergeWithParent(id) => format!("MergeNode    #{id}"),
        _ => "Other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn independent_chain(contents: &[&str]) -> Document {
        let mut weave = IndependentDemoWeave::with_capacity(contents.len(), String::new());
        for (index, contents) in contents.iter().enumerate() {
            let id = index as u64;
            assert!(weave.insert(IndependentDemoNode {
                id,
                from: (id > 0).then(|| id - 1).into_iter().collect(),
                to: IndexSet::default(),
                active: true,
                bookmarked: false,
                contents: TextContent((*contents).to_string()),
            }));
        }
        Document::new_independent(weave)
    }

    fn replace(document: &mut Document, edited: &str, next_id: &mut u64) -> bool {
        let (path, text) = document.active_path_text();
        document
            .replace_active_path_text(&path, &text, edited, next_id, 1)
            .unwrap()
            .is_some()
    }

    /// The old and new text of every hunk between `original` and `edited`.
    fn hunk_texts<'a>(original: &'a str, edited: &'a str) -> Vec<(&'a str, &'a str)> {
        text_hunks(original, edited)
            .into_iter()
            .map(|hunk| (&original[hunk.old], &edited[hunk.new]))
            .collect()
    }

    /// The identifier of the one node whose contents equal `contents`.
    fn find(document: &Document, contents: &str) -> u64 {
        let ids: Vec<u64> = (0..=document.max_id().unwrap())
            .filter(|id| document.node_contents(id).as_deref() == Some(contents))
            .collect();
        assert_eq!(
            ids.len(),
            1,
            "expected one node containing {contents:?}, found {ids:?}"
        );
        ids[0]
    }

    /// The number of parent-child edges in the document.
    fn edge_count(document: &Document) -> usize {
        (0..=document.max_id().unwrap())
            .filter_map(|id| document.node_info(&id))
            .map(|info| info.children.len())
            .sum()
    }

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
    fn active_path_replacement_handles_insert_delete_and_replace() {
        for (original, edited) in [
            ("hello world", "hello wide world"),
            ("hello world", "hello "),
            ("hello world", "hello weave"),
        ] {
            let mut document = independent_chain(&[original]);
            let mut next_id = 10;
            assert!(replace(&mut document, edited, &mut next_id));
            assert_eq!(document.active_path_text().1, edited);
            assert!(document.is_valid());
        }
    }

    #[test]
    fn active_path_replacement_handles_full_deletion_and_empty_path_insertion() {
        let mut document = independent_chain(&["remove all of this"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "", &mut next_id));
        assert_eq!(document.active_path_text().1, "");
        assert!(document.contains(&0), "removed text must remain in the DAG");
        assert_eq!(
            document.node_contents(&0).as_deref(),
            Some("remove all of this")
        );
        assert!(document.is_valid());

        let weave = IndependentDemoWeave::with_capacity(1, String::new());
        let mut document = Document::new_independent(weave);
        let mut next_id = 20;
        assert!(replace(
            &mut document,
            "born from an empty path",
            &mut next_id
        ));
        assert_eq!(document.active_path_text().1, "born from an empty path");
        assert!(document.is_valid());
    }

    #[test]
    fn active_path_replacement_crosses_node_and_unicode_boundaries() {
        let mut document = independent_chain(&["one ", "two ", "three"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "one THREE", &mut next_id));
        assert_eq!(document.active_path_text().1, "one THREE");
        assert!(document.is_valid());

        let mut document = independent_chain(&["aé🙂z"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "aê🦀z", &mut next_id));
        assert_eq!(document.active_path_text().1, "aê🦀z");
        assert!(document.is_valid());
    }

    #[test]
    fn replaced_content_survives_on_an_alternate_branch_and_operations_are_logged() {
        let mut document = independent_chain(&["hello cruel world"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "hello kind world", &mut next_id));

        assert_eq!(document.active_path_text().1, "hello kind world");
        assert_eq!(document.node_contents(&11).as_deref(), Some("cruel"));
        assert!(!document.node_info(&11).unwrap().active);

        // The replacement continues only into the shared suffix; it must not become a
        // prefix of the text it replaced, so "hello kind cruel world" is not a path.
        let (head, kind, cruel, tail) = (
            find(&document, "hello "),
            find(&document, "kind"),
            find(&document, "cruel"),
            find(&document, " world"),
        );
        assert_eq!(document.node_info(&kind).unwrap().children, vec![tail]);
        assert_eq!(document.node_info(&cruel).unwrap().parents, vec![head]);
        assert_eq!(document.node_info(&cruel).unwrap().children, vec![tail]);

        let actions = document.formatted_actions().join("\n");
        assert!(actions.contains("SplitNode"));
        assert!(actions.contains("AddNode"));
        assert!(actions.contains("SetActivePath"));
        assert!(document.is_valid());
    }

    #[test]
    fn repeated_insertions_at_one_position_grow_linearly() {
        let mut document = independent_chain(&["hello world"]);
        let mut next_id = 10;
        let edits = [
            "hello A world",
            "hello B A world",
            "hello C B A world",
            "hello D C B A world",
        ];
        for (round, edited) in edits.into_iter().enumerate() {
            assert!(replace(&mut document, edited, &mut next_id));
            assert_eq!(document.active_path_text().1, edited);
            // One edge joins the original halves; each insertion adds exactly two more.
            assert_eq!(edge_count(&document), 1 + 2 * (round + 1));
        }
        // Each inserted word continues only into the word inserted before it.
        let (latest, previous) = (find(&document, "D "), find(&document, "C "));
        assert_eq!(
            document.node_info(&latest).unwrap().children,
            vec![previous]
        );
        assert!(document.is_valid());
    }

    #[test]
    fn separate_changes_become_separate_patches_that_share_unchanged_text() {
        let mut document = independent_chain(&["one two three"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "ONE two THREE", &mut next_id));
        assert_eq!(document.active_path_text().1, "ONE two THREE");

        // The untouched middle is a single node shared by the old and new branches.
        let middle = document.node_info(&find(&document, " two ")).unwrap();
        let (one, capital_one) = (find(&document, "one"), find(&document, "ONE"));
        let (three, capital_three) = (find(&document, "three"), find(&document, "THREE"));
        assert!(middle.parents.contains(&one) && middle.parents.contains(&capital_one));
        assert_eq!(
            middle.children.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([three, capital_three])
        );
        assert!(!document.node_info(&one).unwrap().active);
        assert!(!document.node_info(&three).unwrap().active);
        assert!(document.is_valid());

        // Several hunks spanning several nodes apply in one pass.
        let mut document =
            independent_chain(&["The keeper ", "found the letter ", "on a Tuesday."]);
        let mut next_id = 20;
        let edited = "A keeper found the old letter on a Friday.";
        assert!(replace(&mut document, edited, &mut next_id));
        assert_eq!(document.active_path_text().1, edited);
        assert!(!document.node_info(&find(&document, "The")).unwrap().active);
        assert_eq!(
            document
                .node_info(&find(&document, "old "))
                .unwrap()
                .children
                .len(),
            1
        );
        assert!(document.is_valid());
    }

    #[test]
    fn ambiguous_insertions_align_to_word_boundaries() {
        // "wide " could equally be inserted as "ide w" after "hello w"; prefer the word.
        let mut document = independent_chain(&["hello world"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "hello wide world", &mut next_id));
        assert_eq!(document.active_path_text().1, "hello wide world");
        let (head, inserted, tail) = (
            find(&document, "hello "),
            find(&document, "wide "),
            find(&document, "world"),
        );
        assert_eq!(document.node_info(&inserted).unwrap().parents, vec![head]);
        assert_eq!(document.node_info(&inserted).unwrap().children, vec![tail]);
        assert!(document.is_valid());

        // Appending a repeated word stays at the end instead of sliding to the front.
        let mut document = independent_chain(&["ab ab ab"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "ab ab ab ab", &mut next_id));
        let appended = find(&document, " ab");
        assert_eq!(
            document.node_info(&appended).unwrap().parents,
            vec![find(&document, "ab ab ab")]
        );
        assert!(document.is_valid());
    }

    #[test]
    fn text_hunks_report_every_change_with_character_boundaries() {
        assert_eq!(text_hunks("same", "same"), vec![]);
        assert_eq!(
            text_hunks("one two three", "ONE two THREE"),
            vec![
                TextHunk {
                    old: 0..3,
                    new: 0..3
                },
                TextHunk {
                    old: 8..13,
                    new: 8..13
                },
            ]
        );
        // Whole-word replacement, insertion, and deletion with multi-byte characters.
        assert_eq!(
            text_hunks("aé 🙂 z", "aé 🦀 z"),
            vec![TextHunk {
                old: 4..8,
                new: 4..8
            }]
        );
        assert_eq!(
            text_hunks("aé z", "aé z 🙂"),
            vec![TextHunk {
                old: 5..5,
                new: 5..10
            }]
        );
        assert_eq!(
            text_hunks("aé 🙂 z", "aé 🙂"),
            vec![TextHunk {
                old: 8..10,
                new: 8..8
            }]
        );
    }

    #[test]
    fn text_hunks_widen_partial_word_changes_to_whole_words() {
        // A change inside a word covers the whole word on both branches.
        assert_eq!(
            hunk_texts(
                "The letter came on a Tuesday.",
                "The letter came on a Thursday."
            ),
            vec![("Tuesday", "Thursday")]
        );
        // Adjacent changes inside one word merge into a single hunk.
        assert_eq!(
            hunk_texts("teh cat sat", "the cat sat"),
            vec![("teh", "the")]
        );
        // Continuing a partial word replaces it with the finished word.
        assert_eq!(hunk_texts("The cat sa", "The cat sat"), vec![("sa", "sat")]);
        assert_eq!(
            hunk_texts("hello world", "hello worlds"),
            vec![("world", "worlds")]
        );
        // Changes that already sit on whitespace boundaries are left alone.
        assert_eq!(
            hunk_texts("hello cruel world", "hello kind world"),
            vec![("cruel", "kind")]
        );
        assert_eq!(
            hunk_texts("hello world", "hello wide world"),
            vec![("", "wide ")]
        );
        assert_eq!(
            hunk_texts("line one\nline two\n", "line one\nline 2\nline three\n"),
            vec![("two", "2\nline three")]
        );
        // Widening moves over whole multi-byte characters.
        assert_eq!(hunk_texts("aé🙂z ok", "aê🦀z ok"), vec![("aé🙂z", "aê🦀z")]);
    }

    #[test]
    fn text_hunks_treat_punctuation_as_a_boundary_unless_it_joins_a_word() {
        // Punctuation after a word stays shared instead of joining the replaced word.
        assert_eq!(
            hunk_texts("It rained on Tuesday.", "It rained on Thursday."),
            vec![("Tuesday", "Thursday")]
        );
        assert_eq!(hunk_texts("(hello)", "(hi)"), vec![("hello", "hi")]);
        // Adding or changing punctuation next to a word does not replace the word.
        assert_eq!(hunk_texts("hello world", "hello world!"), vec![("", "!")]);
        assert_eq!(hunk_texts("hello world.", "hello world?"), vec![(".", "?")]);
        assert_eq!(hunk_texts("hello, world", "hello; world"), vec![(",", ";")]);
        // Punctuation joining two word characters belongs to the word.
        assert_eq!(
            hunk_texts("I don't know", "I didn't know"),
            vec![("don't", "didn't")]
        );
        assert_eq!(
            hunk_texts("a well-known fact", "a well known fact"),
            vec![("well-known", "well known")]
        );
        assert_eq!(
            hunk_texts("pi is 3.14", "pi is 3.15"),
            vec![("3.14", "3.15")]
        );
        // The joining rule applies in whichever text the character joins a word.
        assert_eq!(hunk_texts("see a.", "see a.b"), vec![("a.", "a.b")]);
        // Symbols such as emoji follow the same rule as punctuation.
        assert_eq!(hunk_texts("hi🙂", "hi🦀"), vec![("🙂", "🦀")]);
        assert_eq!(hunk_texts("hi 🙂 there", "hi 🦀 there"), vec![("🙂", "🦀")]);
    }

    #[test]
    fn text_hunks_keep_grapheme_clusters_whole() {
        // A combining accent stays with its base character on both branches.
        assert_eq!(
            hunk_texts("cafe\u{301} au lait", "cofe\u{301} au lait"),
            vec![("cafe\u{301}", "cofe\u{301}")]
        );
        // Changing the base character under a combining mark keeps the mark with it.
        assert_eq!(
            hunk_texts("cafe\u{301}", "cafa\u{301}"),
            vec![("cafe\u{301}", "cafa\u{301}")]
        );
        // Changing an emoji modifier replaces the whole cluster.
        assert_eq!(
            hunk_texts("ok \u{1F44D}\u{1F3FD} go", "ok \u{1F44D}\u{1F3FB} go"),
            vec![("\u{1F44D}\u{1F3FD}", "\u{1F44D}\u{1F3FB}")]
        );
        // A flag is one cluster, and a Windows line ending is never split.
        assert_eq!(
            hunk_texts("a \u{1F1FA}\u{1F1F8} b", "a \u{1F1EC}\u{1F1E7} b"),
            vec![("\u{1F1FA}\u{1F1F8}", "\u{1F1EC}\u{1F1E7}")]
        );
        assert_eq!(
            hunk_texts("one\r\ntwo", "one\r\n\r\ntwo"),
            vec![("", "\r\n")]
        );
    }

    /// The spans of a preview as `(range, kind, marks_removal)`, checked to cover the text.
    fn preview_spans(
        original: &str,
        edited: &str,
        node_lengths: &[usize],
    ) -> Vec<(Range<usize>, PreviewKind, bool)> {
        let preview = preview_edit(original, edited, node_lengths);
        let mut expected_start = 0;
        for span in &preview.spans {
            assert_eq!(span.range.start, expected_start, "spans must be contiguous");
            assert!(!span.range.is_empty(), "spans must not be empty");
            assert!(edited.is_char_boundary(span.range.end));
            expected_start = span.range.end;
        }
        assert_eq!(expected_start, edited.len(), "spans must cover the text");
        assert_eq!(preview.changes, text_hunks(original, edited).len());
        preview
            .spans
            .into_iter()
            .map(|span| (span.range, span.kind, span.marks_removal))
            .collect()
    }

    #[test]
    fn edit_previews_attribute_unchanged_text_to_its_node() {
        use PreviewKind::Shared;
        assert_eq!(
            preview_spans("one two three", "one two three", &[4, 4, 5]),
            vec![
                (0..4, Shared { node: 0 }, false),
                (4..8, Shared { node: 1 }, false),
                (8..13, Shared { node: 2 }, false),
            ]
        );
        assert_eq!(preview_edit("one", "one", &[3]).changes, 0);
        // Empty nodes take no span, and their neighbours keep their positions.
        assert_eq!(
            preview_spans("ab", "ab", &[1, 0, 1]),
            vec![
                (0..1, Shared { node: 0 }, false),
                (1..2, Shared { node: 2 }, false)
            ]
        );
        // Lengths that do not add up to the text fall back to a single node.
        assert_eq!(
            preview_spans("abc", "abc", &[1, 1]),
            vec![(0..3, Shared { node: 0 }, false)]
        );
        assert_eq!(preview_spans("", "", &[0]), vec![]);
    }

    #[test]
    fn edit_previews_mark_changes_and_removals() {
        use PreviewKind::{Changed, Shared};
        // A replacement inside a node keeps the boundaries around it in place.
        assert_eq!(
            preview_spans("one two three", "one 2 three", &[4, 4, 5]),
            vec![
                (0..4, Shared { node: 0 }, false),
                (4..5, Changed, false),
                (5..6, Shared { node: 1 }, false),
                (6..11, Shared { node: 2 }, false),
            ]
        );
        // A change spanning a node boundary swallows it.
        assert_eq!(
            preview_spans("one two three", "one TWO THREE", &[4, 4, 5]),
            vec![(0..4, Shared { node: 0 }, false), (4..13, Changed, false)]
        );
        // Text typed after the end is a change, previewed as widened for applying.
        assert_eq!(
            preview_spans("one", "one two", &[3]),
            vec![(0..3, Shared { node: 0 }, false), (3..7, Changed, false)]
        );
        assert_eq!(
            preview_spans("The cat sa", "The cat sat", &[10]),
            vec![(0..8, Shared { node: 0 }, false), (8..11, Changed, false)]
        );
        assert_eq!(
            preview_edit("one two three", "ONE two THREE", &[13]).changes,
            2
        );
        // Removed text has nothing to highlight, so the character next to it is marked:
        // the one after the removal, or the one before it at the end of the text.
        assert_eq!(
            preview_spans("one two", "two", &[4, 3]),
            vec![
                (0..1, Shared { node: 1 }, true),
                (1..3, Shared { node: 1 }, false)
            ]
        );
        assert_eq!(
            preview_spans("one two", "one", &[4, 3]),
            vec![
                (0..2, Shared { node: 0 }, false),
                (2..3, Shared { node: 0 }, true)
            ]
        );
        assert_eq!(
            preview_spans("\u{e9} x", "x", &[4]),
            vec![(0..1, Shared { node: 0 }, true)]
        );
        // Removing everything leaves nothing to mark.
        assert_eq!(preview_spans("gone", "", &[4]), vec![]);
        assert_eq!(preview_edit("gone", "", &[4]).changes, 1);
    }

    #[test]
    fn removal_marks_land_on_visible_characters_around_line_breaks() {
        use PreviewKind::Shared;
        // Removing the end of a line marks the line's last character, since the line
        // break after the removal has no glyph to underline.
        assert_eq!(
            preview_spans("hello world\nnext", "hello\nnext", &[16]),
            vec![
                (0..4, Shared { node: 0 }, false),
                (4..5, Shared { node: 0 }, true),
                (5..10, Shared { node: 0 }, false),
            ]
        );
        // The same applies when the text ends with the line break.
        assert_eq!(
            preview_spans("line\nmore", "line\n", &[9]),
            vec![
                (0..3, Shared { node: 0 }, false),
                (3..4, Shared { node: 0 }, true),
                (4..5, Shared { node: 0 }, false),
            ]
        );
        // A removed line leaves line breaks on both sides; the mark ends the line the
        // removed text followed, or starts the next one at the top of the text.
        assert_eq!(
            preview_spans("a\nXYZ\nb", "a\n\nb", &[7]),
            vec![
                (0..1, Shared { node: 0 }, true),
                (1..4, Shared { node: 0 }, false)
            ]
        );
        assert_eq!(
            preview_spans("XYZ\nb", "\nb", &[5]),
            vec![
                (0..1, Shared { node: 0 }, false),
                (1..2, Shared { node: 0 }, true)
            ]
        );
        // Windows line endings count as line breaks too.
        assert_eq!(
            preview_spans("ab cd\r\nef", "ab\r\nef", &[9]),
            vec![
                (0..1, Shared { node: 0 }, false),
                (1..2, Shared { node: 0 }, true),
                (2..6, Shared { node: 0 }, false),
            ]
        );
        // A removal with nothing visible anywhere near it is still counted.
        assert_eq!(
            preview_spans("\n\n", "\n", &[2]),
            vec![(0..1, Shared { node: 0 }, false)]
        );
        assert_eq!(preview_edit("\n\n", "\n", &[2]).changes, 1);
        // A visible neighbour after the removal is still preferred.
        assert_eq!(
            preview_spans("one\ntwo three", "one\nthree", &[13]),
            vec![
                (0..4, Shared { node: 0 }, false),
                (4..5, Shared { node: 0 }, true),
                (5..9, Shared { node: 0 }, false),
            ]
        );
    }

    #[test]
    fn partial_word_edits_replace_whole_words() {
        let mut document = independent_chain(&["The letter came on a Tuesday."]);
        let mut next_id = 10;
        assert!(replace(
            &mut document,
            "The letter came on a Thursday.",
            &mut next_id
        ));
        assert_eq!(
            document.active_path_text().1,
            "The letter came on a Thursday."
        );
        let (head, old, new, period) = (
            find(&document, "The letter came on a "),
            find(&document, "Tuesday"),
            find(&document, "Thursday"),
            find(&document, "."),
        );
        assert_eq!(document.node_info(&old).unwrap().parents, vec![head]);
        assert_eq!(document.node_info(&new).unwrap().parents, vec![head]);
        assert_eq!(document.node_info(&old).unwrap().children, vec![period]);
        assert_eq!(document.node_info(&new).unwrap().children, vec![period]);
        assert!(!document.node_info(&old).unwrap().active);
        assert!(document.node_info(&new).unwrap().active);
        assert!(document.is_valid());
    }

    #[test]
    fn appending_fills_an_empty_placeholder_tip() {
        // A childless empty tip, such as a node just added from the inspector, takes
        // the appended text instead of being left dangling behind a new node.
        let mut document = independent_chain(&["hello"]);
        assert!(document.add_child(&0, 1));
        let (path, text) = document.active_path_text();
        let mut next_id = 10;
        let summary = document
            .replace_active_path_text(&path, &text, "hello world", &mut next_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(
            document.active_path_text(),
            (vec![1, 0], "hello world".to_string())
        );
        assert_eq!(document.node_contents(&1).as_deref(), Some(" world"));
        assert_eq!(summary.created_nodes, 0);
        assert_eq!(summary.focus, Some(1));
        assert_eq!(next_id, 10);
        assert!(document.is_valid());

        // Typing into a fully deleted path refills its empty root.
        let mut document = independent_chain(&["hello"]);
        assert!(replace(&mut document, "", &mut next_id));
        let nodes = document.len();
        assert!(replace(&mut document, "new", &mut next_id));
        assert_eq!(document.len(), nodes);
        assert_eq!(document.active_path_text().1, "new");
        assert!(document.is_valid());

        // An empty tip that other branches continue from is left alone.
        let mut document = independent_chain(&["hello", "", "!"]);
        assert!(document.set_inactive(&2));
        assert_eq!(document.active_path(), vec![1, 0]);
        assert!(replace(&mut document, "hello world", &mut next_id));
        assert_eq!(document.node_contents(&1).as_deref(), Some(""));
        assert_eq!(document.active_path_text().1, "hello world");
        assert!(document.is_valid());
    }

    #[test]
    fn applied_edits_report_their_size_and_first_change() {
        let mut document = independent_chain(&["one two three"]);
        let (path, text) = document.active_path_text();
        let mut next_id = 10;
        let summary = document
            .replace_active_path_text(&path, &text, "ONE two THREE", &mut next_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(summary.hunks, 2);
        assert_eq!(summary.created_nodes, document.len() - 1);
        assert_eq!(summary.focus, Some(find(&document, "ONE")));

        // A deletion at the end focuses the text that now ends the path.
        let (path, text) = document.active_path_text();
        let nodes = document.len();
        let summary = document
            .replace_active_path_text(&path, &text, "ONE two", &mut next_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(summary.hunks, 1);
        assert_eq!(summary.created_nodes, document.len() - nodes);
        assert_eq!(summary.focus, Some(find(&document, " two")));
        assert!(document.is_valid());
    }

    #[test]
    fn changing_text_back_reuses_the_existing_branch() {
        let mut document = independent_chain(&["hello cruel world"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "hello kind world", &mut next_id));
        assert_eq!(next_id, 13);
        let nodes = document.len();
        let (cruel, kind) = (find(&document, "cruel"), find(&document, "kind"));
        assert!(!document.node_info(&cruel).unwrap().active);

        let (path, text) = document.active_path_text();
        let summary = document
            .replace_active_path_text(&path, &text, "hello cruel world", &mut next_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(summary.hunks, 1);
        assert_eq!(summary.reused_branches, 1);
        assert_eq!(summary.created_nodes, 0);
        assert_eq!(summary.focus, Some(cruel));
        assert_eq!(document.len(), nodes, "no duplicate node was created");
        assert_eq!(next_id, 13, "no identifiers were consumed");
        assert_eq!(document.active_path_text().1, "hello cruel world");
        assert!(document.node_info(&cruel).unwrap().active);
        assert!(!document.node_info(&kind).unwrap().active);
        assert!(
            document
                .formatted_actions()
                .join("\n")
                .contains("SetActivePath")
        );
        assert!(document.is_valid());

        // Switching between earlier versions never grows the graph.
        for edited in ["hello kind world", "hello cruel world", "hello kind world"] {
            assert!(replace(&mut document, edited, &mut next_id));
            assert_eq!(document.active_path_text().1, edited);
            assert_eq!(document.len(), nodes);
        }
        assert!(document.is_valid());
    }

    #[test]
    fn reuse_covers_multi_node_branches_insertions_and_roots() {
        // A replacement spelled out by a chain of existing nodes.
        let mut document = independent_chain(&["one ", "two ", "three"]);
        let mut next_id = 10;
        assert!(replace(&mut document, "one X", &mut next_id));
        let nodes = document.len();
        assert!(replace(&mut document, "one two three", &mut next_id));
        assert_eq!(document.active_path(), vec![2, 1, 0]);
        assert_eq!(document.len(), nodes);
        assert!(document.is_valid());

        // An insertion that an earlier edit deleted again.
        let mut document = independent_chain(&["hello world"]);
        assert!(replace(&mut document, "hello wide world", &mut next_id));
        assert!(replace(&mut document, "hello world", &mut next_id));
        let nodes = document.len();
        let wide = find(&document, "wide ");
        assert!(!document.node_info(&wide).unwrap().active);
        assert!(replace(&mut document, "hello wide world", &mut next_id));
        assert_eq!(document.len(), nodes);
        assert!(document.node_info(&wide).unwrap().active);
        assert_eq!(document.active_path_text().1, "hello wide world");
        assert!(document.is_valid());

        // A replaced root.
        let mut document = independent_chain(&["hello", " world"]);
        assert!(replace(&mut document, "hi world", &mut next_id));
        let nodes = document.len();
        let hi = find(&document, "hi");
        assert!(document.node_info(&hi).unwrap().parents.is_empty());
        assert!(replace(&mut document, "hello world", &mut next_id));
        assert_eq!(document.len(), nodes);
        assert_eq!(document.active_path(), vec![1, 0]);
        assert!(!document.node_info(&hi).unwrap().active);
        assert!(document.is_valid());
    }

    #[test]
    fn branches_with_a_different_continuation_are_not_reused() {
        let mut weave = IndependentDemoWeave::with_capacity(8, String::new());
        let node = |id: u64, parents: &[u64], active: bool, text: &str| IndependentDemoNode {
            id,
            from: parents.iter().copied().collect(),
            to: IndexSet::default(),
            active,
            bookmarked: false,
            contents: TextContent(text.to_string()),
        };
        assert!(weave.insert(node(0, &[], true, "a ")));
        assert!(weave.insert(node(1, &[0], true, "b")));
        assert!(weave.insert(node(2, &[1], true, " c")));
        // "x" continues into " z", not into " c", so "a x c" is not an existing path.
        assert!(weave.insert(node(3, &[0], false, "x")));
        assert!(weave.insert(node(4, &[3], false, " z")));
        let mut document = Document::new_independent(weave);

        let (path, text) = document.active_path_text();
        assert_eq!(text, "a b c");
        let mut next_id = 10;
        let summary = document
            .replace_active_path_text(&path, &text, "a x c", &mut next_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(summary.reused_branches, 0);
        assert_eq!(summary.created_nodes, 1);
        assert_eq!(document.active_path_text().1, "a x c");
        assert!(!document.node_info(&3).unwrap().active);
        assert_eq!(document.node_info(&2).unwrap().parents, vec![1, 10]);
        assert!(document.is_valid());
    }

    #[test]
    fn paths_can_be_restored_while_their_nodes_still_form_a_chain() {
        let mut document = seeded_independent();
        let original = document.active_path();
        assert_eq!(original, vec![3, 1, 0]);
        assert!(document.set_active(&4));
        assert_eq!(document.active_path(), vec![4, 2, 0]);
        assert!(document.can_restore_path(&original));
        document.restore_path(&original).unwrap();
        assert_eq!(document.active_path(), original);
        assert_eq!(document.active_set(), HashSet::from([0, 1, 3]));
        assert!(document.is_valid());

        // An empty path deactivates everything.
        document.restore_path(&[]).unwrap();
        assert!(document.active_path().is_empty());
        assert!(document.is_valid());

        // Broken chains, non-roots, and removed nodes are refused.
        assert!(!document.can_restore_path(&[3, 0]));
        assert!(!document.can_restore_path(&[1]));
        assert_eq!(document.remove(&1), Some(2));
        assert!(!document.can_restore_path(&original));
        let error = document.restore_path(&original).unwrap_err();
        assert!(error.contains("can no longer be restored"));

        // Tree documents restore a path through its tip.
        let mut document = seeded_dependent();
        assert!(document.set_active(&4));
        document.restore_path(&[3, 1, 0]).unwrap();
        assert_eq!(document.active_path(), vec![3, 1, 0]);
        assert!(document.is_valid());
    }

    #[test]
    fn unchanged_and_stale_path_edits_are_safe() {
        let mut document = independent_chain(&["first", " second"]);
        let (path, text) = document.active_path_text();
        let action_count = document.action_count();
        let mut next_id = 10;
        assert!(
            document
                .replace_active_path_text(&path, &text, &text, &mut next_id, 1)
                .unwrap()
                .is_none()
        );
        assert_eq!(document.action_count(), action_count);
        assert_eq!(next_id, 10);

        assert!(document.set_inactive(&1));
        let error = document
            .replace_active_path_text(&path, &text, "stale", &mut next_id, 1)
            .unwrap_err();
        assert!(error.contains("changed while this edit was staged"));
        assert_eq!(document.active_path_text().1, "first");
        assert!(document.is_valid());
    }

    #[test]
    fn identifier_exhaustion_does_not_partially_patch() {
        // Replacing the middle word splits both neighbours, so the patch needs several
        // identifiers while the sequence can only produce one more.
        let mut document = independent_chain(&["one two three"]);
        let (path, text) = document.active_path_text();
        let mut next_id = u64::MAX;
        let error = document
            .replace_active_path_text(&path, &text, "one TWO three", &mut next_id, 1)
            .unwrap_err();
        assert!(error.contains("Identifier sequence exhausted"));
        assert_eq!(document.active_path_text().1, text);
        assert!(document.is_valid());
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

        weave.insert(node(0, &[]));
        weave.insert(node(1, &[0]));
        weave.insert(node(2, &[0]));
        weave.insert(node(3, &[1]));

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

        weave.insert(node(0, &[]));
        weave.insert(node(1, &[0]));
        weave.insert(node(2, &[0]));
        weave.insert(node(3, &[1, 2])); // shared child

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
