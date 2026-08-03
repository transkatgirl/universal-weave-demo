//! Node contents and shared type aliases for the demo.

use std::hash::RandomState;

use rkyv::{Archive, Deserialize, Serialize};
use universal_weave::{
    DeduplicatableContents, DiscreteContentResult, DiscreteContents, IndependentContents,
    dependent::{DependentNode, DependentWeave, loro::DependentLoroWeave},
    independent::{IndependentNode, IndependentWeave},
};

/// The tree-based weave type used by the demo: text nodes with a document title as metadata.
pub type DemoWeave = DependentWeave<u64, TextContent, String, RandomState>;
/// The tree-based node type used by the demo.
pub type DemoNode = DependentNode<u64, TextContent, RandomState>;
/// The collaborative tree-based weave type used by the demo.
pub type CollaborativeDemoWeave = DependentLoroWeave<u64, TextContent, String, RandomState>;
/// The DAG-based weave type used by the demo: text nodes with a document title as metadata.
pub type IndependentDemoWeave = IndependentWeave<u64, TextContent, String, RandomState>;
/// The DAG-based node type used by the demo.
pub type IndependentDemoNode = IndependentNode<u64, TextContent, RandomState>;

/// Plain text node contents.
#[derive(
    Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize, Deserialize,
)]
pub struct TextContent(pub String);

impl IndependentContents for TextContent {}

impl DiscreteContents for TextContent {
    fn split(self, at: usize) -> DiscreteContentResult<Self> {
        if at == 0 || at >= self.0.len() || !self.0.is_char_boundary(at) {
            return DiscreteContentResult::One(self);
        }

        let (left, right) = self.0.split_at(at);

        DiscreteContentResult::Two(TextContent(left.to_owned()), TextContent(right.to_owned()))
    }

    fn merge(self, value: Self) -> DiscreteContentResult<Self> {
        if self.0.is_empty() {
            return DiscreteContentResult::One(value);
        }
        if value.0.is_empty() {
            return DiscreteContentResult::One(self);
        }

        let mut merged = self.0;
        merged.push_str(&value.0);

        DiscreteContentResult::One(TextContent(merged))
    }
}

impl DeduplicatableContents for TextContent {
    fn is_duplicate_of(&self, other: &Self) -> bool {
        !self.0.is_empty() && self.0 == other.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_merge_roundtrip() {
        let content = TextContent("hello world".to_string());

        let DiscreteContentResult::Two(left, right) = content.split(5) else {
            panic!("split failed");
        };
        assert_eq!(left.0, "hello");
        assert_eq!(right.0, " world");

        let DiscreteContentResult::One(merged) = left.merge(right) else {
            panic!("merge failed");
        };
        assert_eq!(merged.0, "hello world");
    }

    #[test]
    fn split_respects_char_boundaries() {
        // 'é' is two bytes wide; byte index 2 falls in the middle of it.
        let content = TextContent("héllo".to_string());

        let DiscreteContentResult::One(unchanged) = content.split(2) else {
            panic!("split should have failed on a non-boundary index");
        };
        assert_eq!(unchanged.0, "héllo");
    }

    #[test]
    fn split_rejects_edges() {
        let content = TextContent("abc".to_string());

        let DiscreteContentResult::One(unchanged) = content.split(0) else {
            panic!("split should have failed at index 0");
        };
        assert_eq!(unchanged.0, "abc");

        let content = TextContent("abc".to_string());
        let DiscreteContentResult::One(unchanged) = content.split(3) else {
            panic!("split should have failed at the end");
        };
        assert_eq!(unchanged.0, "abc");
    }
}
