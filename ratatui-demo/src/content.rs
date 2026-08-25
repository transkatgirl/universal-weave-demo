//! Text contents and weave aliases used by the demo.

use std::hash::RandomState;

use rkyv::{Archive, Deserialize, Serialize};
use universal_weave::{
    DeduplicatableContents, DiscreteContentResult, DiscreteContents, IndependentContents,
    dependent::{DependentNode, DependentWeave},
    independent::{IndependentNode, IndependentWeave},
};

pub type DemoWeave = DependentWeave<u64, TextContent, String, RandomState>;
pub type DemoNode = DependentNode<u64, TextContent, RandomState>;
pub type IndependentDemoWeave = IndependentWeave<u64, TextContent, String, RandomState>;
pub type IndependentDemoNode = IndependentNode<u64, TextContent, RandomState>;

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
        DiscreteContentResult::Two(Self(left.to_owned()), Self(right.to_owned()))
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
        DiscreteContentResult::One(Self(merged))
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
        let content = TextContent("hello world".to_owned());
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
    fn split_rejects_edges_and_non_char_boundaries() {
        for at in [0, 2, 6] {
            let value = TextContent("héllo".to_owned());
            let DiscreteContentResult::One(unchanged) = value.split(at) else {
                panic!("split at {at} should fail");
            };
            assert_eq!(unchanged.0, "héllo");
        }
    }
}
