//! A uniform document API over dependent and independent weaves.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use hashbrown::HashSet;

use universal_weave::indexmap::IndexSet;
use universal_weave::wrappers::CountedWeave;
use universal_weave::{
    ActivePathWeave, ActiveSingularWeave, BookmarkableWeave, DiscreteWeave, IndependentWeave,
    MetadataWeave, SemiIndependentWeave, SortableWeave, Weave,
};

use super::content::{DemoNode, DemoWeave, IndependentDemoNode, IndependentDemoWeave, TextContent};
use super::tree_view::{self, TreeLayout, TreeNode};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WeaveKind {
    #[default]
    Dependent,
    Independent,
}

impl WeaveKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dependent => "DependentWeave (tree)",
            Self::Independent => "IndependentWeave (DAG)",
        }
    }

    #[cfg(test)]
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Dependent => "Dependent",
            Self::Independent => "Independent",
        }
    }
}

pub(crate) type CountedDependent = CountedWeave<DemoWeave, u64, DemoNode, TextContent>;
pub(crate) type CountedIndependent =
    CountedWeave<IndependentDemoWeave, u64, IndependentDemoNode, TextContent>;

pub enum Document {
    Dependent(Box<CountedDependent>),
    Independent(Box<CountedIndependent>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub parents: Vec<u64>,
    pub children: Vec<u64>,
    pub active: bool,
    pub bookmarked: bool,
    pub content_len: usize,
}

impl Document {
    pub fn new_dependent(weave: DemoWeave) -> Self {
        Self::Dependent(Box::new(CountedWeave::from(weave)))
    }

    pub fn new_independent(weave: IndependentDemoWeave) -> Self {
        Self::Independent(Box::new(CountedWeave::from(weave)))
    }

    pub fn empty(kind: WeaveKind) -> Self {
        match kind {
            WeaveKind::Dependent => {
                let mut weave = DemoWeave::with_capacity(8, "Untitled document".to_owned());
                weave.insert(DemoNode {
                    id: 0,
                    from: None,
                    to: IndexSet::default(),
                    active: true,
                    bookmarked: false,
                    contents: TextContent::default(),
                });
                Self::new_dependent(weave)
            }
            WeaveKind::Independent => {
                let mut weave =
                    IndependentDemoWeave::with_capacity(8, "Untitled document".to_owned());
                weave.insert(IndependentDemoNode {
                    id: 0,
                    from: IndexSet::default(),
                    to: IndexSet::default(),
                    active: true,
                    bookmarked: false,
                    contents: TextContent::default(),
                });
                Self::new_independent(weave)
            }
        }
    }

    pub const fn kind(&self) -> WeaveKind {
        match self {
            Self::Dependent(_) => WeaveKind::Dependent,
            Self::Independent(_) => WeaveKind::Independent,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Dependent(weave) => weave.len(),
            Self::Independent(weave) => weave.len(),
        }
    }

    #[cfg(test)]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Dependent(weave) => weave.as_weave().validate(),
            Self::Independent(weave) => weave.as_weave().validate(),
        }
    }

    #[cfg(test)]
    pub fn contains(&self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.contains(id),
            Self::Independent(weave) => weave.contains(id),
        }
    }

    pub fn max_id(&self) -> Option<u64> {
        match self {
            Self::Dependent(weave) => weave.nodes().keys().copied().max(),
            Self::Independent(weave) => weave.nodes().keys().copied().max(),
        }
    }

    pub fn metadata(&self) -> &str {
        match self {
            Self::Dependent(weave) => weave.metadata(),
            Self::Independent(weave) => weave.metadata(),
        }
    }

    pub fn set_metadata(&mut self, title: String) {
        match self {
            Self::Dependent(weave) => weave.metadata_mut(|metadata| *metadata = title),
            Self::Independent(weave) => weave.metadata_mut(|metadata| *metadata = title),
        }
    }

    pub fn bookmarks(&self) -> Vec<u64> {
        match self {
            Self::Dependent(weave) => weave.bookmarks().iter().copied().collect(),
            Self::Independent(weave) => weave.bookmarks().iter().copied().collect(),
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
        }
    }

    pub fn node_contents(&self, id: &u64) -> Option<&str> {
        match self {
            Self::Dependent(weave) => weave.get(id).map(|node| node.contents.0.as_str()),
            Self::Independent(weave) => weave.get(id).map(|node| node.contents.0.as_str()),
        }
    }

    pub fn tree_nodes(&mut self) -> Vec<TreeNode> {
        let mut order = Vec::new();
        match self {
            Self::Dependent(weave) => weave.get_ordered_identifiers(&mut order),
            Self::Independent(weave) => weave.get_ordered_identifiers(&mut order),
        }

        order
            .into_iter()
            .filter_map(|id| match self {
                Self::Dependent(weave) => weave.get(&id).map(|node| TreeNode {
                    id,
                    parents: node.from.into_iter().collect(),
                    contents: compact_snippet(&node.contents.0),
                    bookmarked: node.bookmarked,
                }),
                Self::Independent(weave) => weave.get(&id).map(|node| TreeNode {
                    id,
                    parents: node.from.iter().copied().collect(),
                    contents: compact_snippet(&node.contents.0),
                    bookmarked: node.bookmarked,
                }),
            })
            .collect()
    }

    pub fn tree_layout(&mut self) -> TreeLayout {
        match self {
            Self::Dependent(weave) => tree_view::layout::<_, DemoNode, TextContent>(weave.as_mut()),
            Self::Independent(weave) => {
                tree_view::layout::<_, IndependentDemoNode, TextContent>(weave.as_mut())
            }
        }
    }

    pub fn active_set(&self) -> HashSet<u64> {
        match self {
            Self::Dependent(weave) => weave.active().into_iter().collect(),
            Self::Independent(weave) => weave.active().iter().copied().collect(),
        }
    }

    pub fn active_path(&mut self) -> Vec<u64> {
        let mut path = Vec::new();
        match self {
            Self::Dependent(weave) => weave.get_active_path(&mut path),
            Self::Independent(weave) => weave.get_active_path(&mut path),
        }
        path
    }

    pub fn active_tip(&mut self) -> Option<u64> {
        self.active_path().first().copied()
    }

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
        }
    }

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
        }
    }

    pub fn set_active(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.set_active(id, true),
            Self::Independent(weave) => weave.set_active(id, true),
        }
    }

    pub fn set_inactive(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.set_active(id, false),
            Self::Independent(weave) => weave.set_active(id, false),
        }
    }

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
        }
    }

    pub fn apply_edit(&mut self, id: &u64, text: String) -> bool {
        match self {
            Self::Dependent(weave) => weave
                .get_contents_mut(id, |contents| contents.0 = text)
                .is_some(),
            Self::Independent(weave) => weave
                .get_contents_mut(id, |contents| contents.0 = text)
                .is_some(),
        }
    }

    pub fn split(&mut self, id: &u64, at: usize, new_id: u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.split(id, at, new_id),
            Self::Independent(weave) => weave.split(id, at, new_id),
        }
    }

    pub fn merge_with_parent(&mut self, id: &u64) -> Option<u64> {
        match self {
            Self::Dependent(weave) => weave.merge_with_parent(id),
            Self::Independent(weave) => weave.merge_with_parent(id),
        }
    }

    pub fn sort_children(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => {
                weave.sort_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
            Self::Independent(weave) => {
                weave.sort_children_by(id, |a, b| a.contents.0.cmp(&b.contents.0))
            }
        }
    }

    pub fn sort_children_by_id(&mut self, id: &u64) -> bool {
        match self {
            Self::Dependent(weave) => weave.sort_children_by_id(id, Ord::cmp),
            Self::Independent(weave) => weave.sort_children_by_id(id, Ord::cmp),
        }
    }

    pub fn remove(&mut self, id: &u64) -> Option<usize> {
        let mut removed = 0;
        let existed = match self {
            Self::Dependent(weave) => weave.remove_tracked(id, |_| removed += 1),
            Self::Independent(weave) => weave.remove_tracked(id, |_| removed += 1),
        };
        existed.then_some(removed)
    }

    pub fn move_node(&mut self, id: &u64, new_parents: &[u64]) -> Result<(), String> {
        match self {
            Self::Dependent(_) => {
                Err("Moving nodes is only supported by IndependentWeave documents".to_owned())
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
        }
    }

    pub fn reset_action_count(&mut self) {
        match self {
            Self::Dependent(weave) => weave.reset_count(),
            Self::Independent(weave) => weave.reset_count(),
        }
    }
}

fn compact_snippet(text: &str) -> String {
    text.lines().next().unwrap_or("").chars().take(40).collect()
}

#[cfg(test)]
pub fn seeded_dependent() -> Document {
    let mut weave = DemoWeave::with_capacity(16, "The Lighthouse Letter".to_owned());
    let node = |id, from, active, bookmarked, text: &str| DemoNode {
        id,
        from,
        to: IndexSet::default(),
        active,
        bookmarked,
        contents: TextContent(text.to_owned()),
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

#[cfg(test)]
pub fn seeded_independent() -> Document {
    let Document::Dependent(counted) = seeded_dependent() else {
        unreachable!();
    };
    Document::new_independent(IndependentDemoWeave::from((*counted).into_weave()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn seeded_documents_are_valid() {
        let mut dependent = seeded_dependent();
        assert!(dependent.is_valid());
        assert_eq!(dependent.len(), 5);
        assert_eq!(dependent.active_tip(), Some(3));
        assert_eq!(dependent.bookmarks(), vec![1]);

        let mut independent = seeded_independent();
        assert!(independent.is_valid());
        assert_eq!(independent.active_tip(), Some(3));
        assert_eq!(independent.active_set(), HashSet::from([0, 1, 3]));
    }

    #[test]
    fn both_empty_document_kinds_support_core_operations() {
        for kind in [WeaveKind::Dependent, WeaveKind::Independent] {
            let mut document = Document::empty(kind);
            assert!(document.add_child(&0, 2));
            assert!(document.add_child(&0, 1));
            assert_eq!(document.node_info(&0).unwrap().children, vec![2, 1]);
            assert!(document.sort_children_by_id(&0));
            assert_eq!(document.node_info(&0).unwrap().children, vec![1, 2]);
            assert!(document.toggle_active(&1));
            assert!(document.set_bookmarked(&1, true));
            assert!(document.apply_edit(&1, "tail".to_owned()));
            assert!(document.is_valid());
        }
    }

    #[test]
    fn independent_move_rejects_cycles() {
        let mut document = Document::empty(WeaveKind::Independent);
        assert!(document.add_child(&0, 1));
        assert!(document.add_child(&0, 2));
        assert!(document.add_child(&1, 3));
        document.move_node(&3, &[2]).unwrap();
        assert_eq!(document.node_info(&3).unwrap().parents, vec![2]);
        assert!(document.move_node(&3, &[3]).is_err());
        assert!(document.move_node(&0, &[3]).is_err());
        assert!(document.is_valid());
    }

    #[test]
    fn independent_remove_preserves_shared_child() {
        let mut document = Document::empty(WeaveKind::Independent);
        assert!(document.add_child(&0, 1));
        assert!(document.add_child(&0, 2));
        assert!(document.add_child(&1, 3));
        document.move_node(&3, &[1, 2]).unwrap();
        assert_eq!(document.remove(&1), Some(1));
        assert_eq!(document.node_info(&3).unwrap().parents, vec![2]);
        assert!(document.is_valid());
    }
}
