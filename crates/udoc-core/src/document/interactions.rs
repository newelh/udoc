//! Interactions overlay: forms, comments, tracked changes.
//!
//! User-interactive elements that do not affect the content spine.
//! Items carry their own geometry and page index because they are
//! visual widgets (PDF form fields have bounding boxes on pages).

use crate::geometry::BoundingBox;

use super::content::Inline;
use super::NodeId;

/// Interactions overlay: forms, comments, tracked changes.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Interactions {
    pub form_fields: Vec<FormField>,
    pub comments: Vec<Comment>,
    pub tracked_changes: Vec<TrackedChange>,
}

impl Interactions {
    /// Whether the named node anchors any form field, comment, or
    /// tracked change in this overlay. Drives
    /// [`crate::document::Document::interactions_for`] (
    ///).
    pub fn has_node(&self, node: NodeId) -> bool {
        self.form_fields.iter().any(|f| f.anchor == Some(node))
            || self.comments.iter().any(|c| c.anchor == node)
            || self.tracked_changes.iter().any(|t| t.anchor == node)
    }
}

/// A form field.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct FormField {
    pub anchor: Option<NodeId>,
    pub name: String,
    pub field_type: FormFieldType,
    pub value: Option<String>,
    pub bbox: Option<BoundingBox>,
    pub page_index: Option<usize>,
}

/// Form field type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum FormFieldType {
    Text,
    Checkbox,
    Radio,
    Dropdown,
    Signature,
    Button,
}

/// Maximum nesting depth for comment replies. Prevents stack overflow
/// from adversarial JSON with deeply nested reply chains.
///
/// not part of the public API. Re-exported as
/// `pub` from `udoc_core::document` only under the `test-internals`
/// feature. The `#[allow(dead_code)]` covers the build matrix where
/// neither `serde` (the only consumer of this constant inside the
/// crate) nor `test-internals` is enabled.
#[cfg(feature = "test-internals")]
pub const MAX_COMMENT_DEPTH: usize = 64;
#[cfg(not(feature = "test-internals"))]
#[allow(dead_code)]
pub(crate) const MAX_COMMENT_DEPTH: usize = 64;

/// A comment anchored to a node.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Comment {
    pub anchor: NodeId,
    pub author: Option<String>,
    pub date: Option<String>,
    pub text: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, deserialize_with = "deserialize_replies")
    )]
    pub replies: Vec<Comment>,
    /// For PDF annotations that span a rectangle rather than anchor to a node.
    pub bbox: Option<BoundingBox>,
    pub page_index: Option<usize>,
}

// Thread-local depth counter for Comment deserialization.
// Incremented when entering a replies array, decremented when leaving.
// Uses an RAII guard to ensure the counter is restored even on panic.
#[cfg(feature = "serde")]
std::thread_local! {
    static COMMENT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// RAII guard that restores the comment depth counter on drop (including
/// during unwinding). Prevents the thread-local from being permanently
/// elevated if deserialization panics.
#[cfg(feature = "serde")]
struct CommentDepthGuard {
    prev: usize,
}

#[cfg(feature = "serde")]
impl Drop for CommentDepthGuard {
    fn drop(&mut self) {
        COMMENT_DEPTH.with(|d| d.set(self.prev));
    }
}

#[cfg(feature = "serde")]
fn deserialize_replies<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Comment>, D::Error> {
    let depth = COMMENT_DEPTH.with(|d| d.get());
    if depth >= MAX_COMMENT_DEPTH {
        return Err(serde::de::Error::custom(format!(
            "comment reply nesting exceeds maximum depth ({})",
            MAX_COMMENT_DEPTH
        )));
    }
    COMMENT_DEPTH.with(|d| d.set(depth + 1));
    let _guard = CommentDepthGuard { prev: depth };
    <Vec<Comment> as serde::Deserialize>::deserialize(deserializer)
    // _guard restores depth on drop (normal return or panic)
}

/// A tracked change (insertion, deletion, format change).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct TrackedChange {
    pub anchor: NodeId,
    pub change_type: ChangeType,
    pub author: Option<String>,
    pub date: Option<String>,
    pub old_content: Option<Vec<Inline>>,
}

/// Type of tracked change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum ChangeType {
    Insertion,
    Deletion,
    FormatChange,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactions_default() {
        let i = Interactions::default();
        assert!(i.form_fields.is_empty());
        assert!(i.comments.is_empty());
        assert!(i.tracked_changes.is_empty());
    }

    #[test]
    fn form_field_types() {
        assert_eq!(FormFieldType::Text, FormFieldType::Text);
        assert_ne!(FormFieldType::Text, FormFieldType::Checkbox);
    }

    #[test]
    fn change_types() {
        assert_eq!(ChangeType::Insertion, ChangeType::Insertion);
        assert_ne!(ChangeType::Insertion, ChangeType::Deletion);
    }

    #[test]
    fn comment_with_replies() {
        let comment = Comment {
            anchor: NodeId::new(1),
            author: Some("Alice".into()),
            date: Some("2026-03-09".into()),
            text: "Looks good".into(),
            replies: vec![Comment {
                anchor: NodeId::new(1),
                author: Some("Bob".into()),
                date: None,
                text: "Thanks".into(),
                replies: vec![],
                bbox: None,
                page_index: None,
            }],
            bbox: None,
            page_index: Some(0),
        };
        assert_eq!(comment.replies.len(), 1);
        assert_eq!(comment.replies[0].text, "Thanks");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn comment_serde_roundtrip() {
        let comment = Comment {
            anchor: NodeId::new(1),
            author: Some("Alice".into()),
            date: Some("2026-03-09".into()),
            text: "Looks good".into(),
            replies: vec![Comment {
                anchor: NodeId::new(1),
                author: Some("Bob".into()),
                date: None,
                text: "Thanks".into(),
                replies: vec![],
                bbox: None,
                page_index: None,
            }],
            bbox: None,
            page_index: Some(0),
        };
        let json = serde_json::to_string(&comment).unwrap();
        let deserialized: Comment = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.text, "Looks good");
        assert_eq!(deserialized.replies.len(), 1);
        assert_eq!(deserialized.replies[0].text, "Thanks");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn comment_depth_limit_enforced() {
        // MAX_COMMENT_DEPTH is 64, but each Comment nests ~3 JSON levels,
        // so serde_json's own 128-level recursion limit fires at ~42 Comment
        // levels, before our custom limit. Our depth counter still guards
        // against serde implementations with higher recursion limits.
        // This test verifies the counter works at depths below both limits.

        // Build 30 levels of nesting (under serde_json's 128 level limit,
        // 30 * ~3 = ~90 JSON levels)
        let mut json = r#"{"anchor":0,"text":"leaf","replies":[]}"#.to_string();
        for i in (0..30).rev() {
            json = format!(
                r#"{{"anchor":0,"text":"depth-{}","replies":[{}]}}"#,
                i, json
            );
        }
        // 30 levels should succeed (under MAX_COMMENT_DEPTH of 64)
        let result = serde_json::from_str::<Comment>(&json);
        assert!(result.is_ok(), "30 levels should be within limit");

        // Verify the depth counter resets properly (not leaking across calls)
        let shallow = r#"{"anchor":0,"text":"shallow","replies":[]}"#;
        let result = serde_json::from_str::<Comment>(shallow);
        assert!(result.is_ok(), "shallow comment should work after deep one");
    }

    #[test]
    fn tracked_change_deletion() {
        let change = TrackedChange {
            anchor: NodeId::new(5),
            change_type: ChangeType::Deletion,
            author: Some("Editor".into()),
            date: None,
            old_content: Some(vec![Inline::Text {
                id: NodeId::new(6),
                text: "deleted text".into(),
                style: crate::document::content::SpanStyle::default(),
            }]),
        };
        assert_eq!(change.change_type, ChangeType::Deletion);
        assert!(change.old_content.is_some());
    }
}
