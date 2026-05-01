//! Overlay types for per-node data indexed by NodeId.
//!
//! Two implementations with identical APIs:
//! - [`Overlay<T>`]: Dense `Vec<Option<T>>` for data present on most nodes.
//! - [`SparseOverlay<T>`]: `HashMap<NodeId, T>` for data present on few nodes.

use std::collections::HashMap;

use super::{NodeId, MAX_NODE_ID};

/// A `Vec<Option<T>>` indexed by NodeId. O(1) access, no hashing.
///
/// Best for dense overlays where most nodes have data (e.g., page
/// assignments, geometry on PDF pages).
///
/// ```
/// use udoc_core::document::{Document, Overlay};
/// let doc = Document::new();
/// let mut scores: Overlay<f64> = Overlay::new();
/// let id = doc.alloc_node_id();
/// scores.set(id, 0.95);
/// assert_eq!(scores.get(id), Some(&0.95));
/// ```
#[derive(Debug, Clone)]
pub struct Overlay<T> {
    data: Vec<Option<T>>,
    /// Cached count of non-None entries. Maintained by set/remove.
    count: usize,
}

impl<T> Overlay<T> {
    /// Create an empty overlay.
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            count: 0,
        }
    }

    /// Look up the value for a node. Returns None if the NodeId is out of
    /// range or has no value set.
    pub fn get(&self, id: NodeId) -> Option<&T> {
        self.data.get(id.value() as usize).and_then(|v| v.as_ref())
    }

    /// Look up the value for a node mutably.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut T> {
        self.data
            .get_mut(id.value() as usize)
            .and_then(|v| v.as_mut())
    }

    /// Set a value for a node. Extends the backing vec if needed.
    ///
    /// # Panics
    /// Panics if the NodeId exceeds 16 million (safety limit to prevent
    /// accidental OOM from absurdly large NodeIds). Prefer [`try_set`](Overlay::try_set)
    /// in production code handling untrusted input.
    pub fn set(&mut self, id: NodeId, value: T) {
        let idx = id.value() as usize;
        assert!(
            id.value() < MAX_NODE_ID,
            "Overlay: NodeId {} exceeds maximum ({})",
            id,
            MAX_NODE_ID
        );
        if idx >= self.data.len() {
            self.data.resize_with(idx + 1, || None);
        }
        if self.data[idx].is_none() {
            self.count += 1;
        }
        self.data[idx] = Some(value);
    }

    /// Try to set a value for a node. Returns `Err(value)` if the NodeId
    /// exceeds the safety limit (16 million).
    pub fn try_set(&mut self, id: NodeId, value: T) -> std::result::Result<(), T> {
        if id.value() >= MAX_NODE_ID {
            return Err(value);
        }
        let idx = id.value() as usize;
        if idx >= self.data.len() {
            self.data.resize_with(idx + 1, || None);
        }
        if self.data[idx].is_none() {
            self.count += 1;
        }
        self.data[idx] = Some(value);
        Ok(())
    }

    /// Remove the value for a node, returning it if present.
    pub fn remove(&mut self, id: NodeId) -> Option<T> {
        let idx = id.value() as usize;
        if idx < self.data.len() {
            let taken = self.data[idx].take();
            if taken.is_some() {
                self.count -= 1;
            }
            taken
        } else {
            None
        }
    }

    /// Whether a value exists for the given node.
    pub fn contains(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    /// Iterate over all (NodeId, &T) pairs where a value is present.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &T)> {
        self.data
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_ref().map(|val| (NodeId::new(i as u64), val)))
    }

    /// Number of nodes with values (not the backing vec length). O(1).
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether no nodes have values. O(1).
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl<T> Default for Overlay<T> {
    fn default() -> Self {
        Self::new()
    }
}

// Custom serde: serialize as an object with string keys (NodeId as string),
// only non-None entries. e.g. {"0": value0, "3": value3}
#[cfg(feature = "serde")]
impl<T: serde::Serialize> serde::Serialize for Overlay<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.count))?;
        for (i, val) in self.data.iter().enumerate() {
            if let Some(v) = val {
                map.serialize_entry(&i.to_string(), v)?;
            }
        }
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Overlay<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;
        use std::marker::PhantomData;

        struct OverlayVisitor<T>(PhantomData<T>);

        impl<'de, T: serde::Deserialize<'de>> Visitor<'de> for OverlayVisitor<T> {
            type Value = Overlay<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map with string keys representing NodeIds")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut overlay = Overlay::new();
                while let Some((key, value)) = access.next_entry::<String, T>()? {
                    let idx: u64 = key.parse().map_err(serde::de::Error::custom)?;
                    if idx >= super::MAX_NODE_ID {
                        return Err(serde::de::Error::custom(format!(
                            "NodeId {} exceeds maximum ({})",
                            idx,
                            super::MAX_NODE_ID
                        )));
                    }
                    overlay.set(NodeId::new(idx), value);
                }
                Ok(overlay)
            }
        }

        deserializer.deserialize_map(OverlayVisitor(PhantomData))
    }
}

/// A `HashMap<NodeId, T>` with the same API as `Overlay<T>`.
///
/// Best for sparse overlays where few nodes have data (e.g.,
/// ExtendedTextStyle on a DOCX where only 200 of 50,000 spans have
/// non-default styling).
#[derive(Debug, Clone)]
pub struct SparseOverlay<T> {
    data: HashMap<NodeId, T>,
}

impl<T> SparseOverlay<T> {
    /// Create an empty sparse overlay.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Look up the value for a node.
    pub fn get(&self, id: NodeId) -> Option<&T> {
        self.data.get(&id)
    }

    /// Look up the value for a node mutably.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut T> {
        self.data.get_mut(&id)
    }

    /// Set a value for a node.
    ///
    /// # Panics
    /// Panics if NodeId exceeds the safety limit (16 million). Prefer
    /// [`try_set`](SparseOverlay::try_set) in production code handling untrusted input.
    pub fn set(&mut self, id: NodeId, value: T) {
        assert!(
            id.value() < MAX_NODE_ID,
            "SparseOverlay: NodeId {} exceeds maximum ({})",
            id,
            MAX_NODE_ID
        );
        self.data.insert(id, value);
    }

    /// Try to set a value for a node. Returns `Err(value)` if the NodeId
    /// exceeds the safety limit (16 million).
    pub fn try_set(&mut self, id: NodeId, value: T) -> std::result::Result<(), T> {
        if id.value() >= MAX_NODE_ID {
            return Err(value);
        }
        self.data.insert(id, value);
        Ok(())
    }

    /// Remove the value for a node, returning it if present.
    pub fn remove(&mut self, id: NodeId) -> Option<T> {
        self.data.remove(&id)
    }

    /// Whether a value exists for the given node.
    pub fn contains(&self, id: NodeId) -> bool {
        self.data.contains_key(&id)
    }

    /// Iterate over all (NodeId, &T) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &T)> {
        self.data.iter().map(|(&id, val)| (id, val))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the overlay is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl<T> Default for SparseOverlay<T> {
    fn default() -> Self {
        Self::new()
    }
}

// Custom serde: same format as Overlay -- object with string keys.
#[cfg(feature = "serde")]
impl<T: serde::Serialize> serde::Serialize for SparseOverlay<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.data.len()))?;
        // Sort by NodeId for deterministic output
        let mut entries: Vec<_> = self.data.iter().collect();
        entries.sort_by_key(|(id, _)| *id);
        for (id, val) in entries {
            map.serialize_entry(&id.value().to_string(), val)?;
        }
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for SparseOverlay<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;
        use std::marker::PhantomData;

        struct SparseVisitor<T>(PhantomData<T>);

        impl<'de, T: serde::Deserialize<'de>> Visitor<'de> for SparseVisitor<T> {
            type Value = SparseOverlay<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map with string keys representing NodeIds")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut overlay = SparseOverlay::new();
                while let Some((key, value)) = access.next_entry::<String, T>()? {
                    let idx: u64 = key.parse().map_err(serde::de::Error::custom)?;
                    if idx >= super::MAX_NODE_ID {
                        return Err(serde::de::Error::custom(format!(
                            "NodeId {} exceeds maximum ({})",
                            idx,
                            super::MAX_NODE_ID
                        )));
                    }
                    overlay.set(NodeId::new(idx), value);
                }
                Ok(overlay)
            }
        }

        deserializer.deserialize_map(SparseVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_basic_operations() {
        let mut o: Overlay<String> = Overlay::new();
        let id0 = NodeId::new(0);
        let id5 = NodeId::new(5);
        let id99 = NodeId::new(99);

        assert!(o.is_empty());
        assert_eq!(o.len(), 0);
        assert_eq!(o.get(id0), None);
        assert_eq!(o.get(id99), None);

        o.set(id5, "hello".into());
        assert_eq!(o.get(id5), Some(&"hello".to_string()));
        assert_eq!(o.len(), 1);
        assert!(!o.is_empty());
        assert!(o.contains(id5));
        assert!(!o.contains(id0));
    }

    #[test]
    fn overlay_get_mut() {
        let mut o: Overlay<i32> = Overlay::new();
        let id = NodeId::new(3);
        o.set(id, 42);
        if let Some(v) = o.get_mut(id) {
            *v = 100;
        }
        assert_eq!(o.get(id), Some(&100));
    }

    #[test]
    fn overlay_remove() {
        let mut o: Overlay<i32> = Overlay::new();
        let id = NodeId::new(2);
        o.set(id, 10);
        assert_eq!(o.remove(id), Some(10));
        assert_eq!(o.get(id), None);
        assert!(o.is_empty());
        // Remove on already-empty slot
        assert_eq!(o.remove(id), None);
        // Remove beyond vec length
        assert_eq!(o.remove(NodeId::new(999)), None);
    }

    #[test]
    fn overlay_iter() {
        let mut o: Overlay<&str> = Overlay::new();
        o.set(NodeId::new(1), "a");
        o.set(NodeId::new(3), "b");
        o.set(NodeId::new(5), "c");

        let mut items: Vec<_> = o.iter().collect();
        items.sort_by_key(|(id, _)| *id);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], (NodeId::new(1), &"a"));
        assert_eq!(items[1], (NodeId::new(3), &"b"));
        assert_eq!(items[2], (NodeId::new(5), &"c"));
    }

    #[test]
    fn overlay_set_extends_vec() {
        let mut o: Overlay<u8> = Overlay::new();
        o.set(NodeId::new(10), 42);
        assert_eq!(o.get(NodeId::new(10)), Some(&42));
        // Intermediate slots should be None
        assert_eq!(o.get(NodeId::new(5)), None);
        assert_eq!(o.len(), 1);
    }

    #[test]
    fn sparse_overlay_basic_operations() {
        let mut s: SparseOverlay<String> = SparseOverlay::new();
        let id0 = NodeId::new(0);
        let id100 = NodeId::new(100);

        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.get(id0), None);

        s.set(id100, "world".into());
        assert_eq!(s.get(id100), Some(&"world".to_string()));
        assert_eq!(s.len(), 1);
        assert!(!s.is_empty());
        assert!(s.contains(id100));
        assert!(!s.contains(id0));
    }

    #[test]
    fn sparse_overlay_get_mut() {
        let mut s: SparseOverlay<i32> = SparseOverlay::new();
        let id = NodeId::new(7);
        s.set(id, 42);
        if let Some(v) = s.get_mut(id) {
            *v = 100;
        }
        assert_eq!(s.get(id), Some(&100));
    }

    #[test]
    fn sparse_overlay_remove() {
        let mut s: SparseOverlay<i32> = SparseOverlay::new();
        let id = NodeId::new(3);
        s.set(id, 10);
        assert_eq!(s.remove(id), Some(10));
        assert_eq!(s.get(id), None);
        assert!(s.is_empty());
        assert_eq!(s.remove(id), None);
    }

    #[test]
    fn sparse_overlay_iter() {
        let mut s: SparseOverlay<&str> = SparseOverlay::new();
        s.set(NodeId::new(10), "x");
        s.set(NodeId::new(20), "y");

        let mut items: Vec<_> = s.iter().collect();
        items.sort_by_key(|(id, _)| *id);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], (NodeId::new(10), &"x"));
        assert_eq!(items[1], (NodeId::new(20), &"y"));
    }

    #[test]
    fn overlay_default() {
        let o: Overlay<i32> = Overlay::default();
        assert!(o.is_empty());
    }

    #[test]
    fn sparse_overlay_default() {
        let s: SparseOverlay<i32> = SparseOverlay::default();
        assert!(s.is_empty());
    }

    #[test]
    fn overlay_try_set_success() {
        let mut o: Overlay<i32> = Overlay::new();
        assert!(o.try_set(NodeId::new(5), 42).is_ok());
        assert_eq!(o.get(NodeId::new(5)), Some(&42));
        assert_eq!(o.len(), 1);
    }

    #[test]
    fn overlay_try_set_rejects_over_max() {
        let mut o: Overlay<i32> = Overlay::new();
        let result = o.try_set(NodeId::new(MAX_NODE_ID), 42);
        assert_eq!(result, Err(42));
        assert!(o.is_empty());
    }

    #[test]
    fn sparse_overlay_try_set_success() {
        let mut s: SparseOverlay<i32> = SparseOverlay::new();
        assert!(s.try_set(NodeId::new(5), 42).is_ok());
        assert_eq!(s.get(NodeId::new(5)), Some(&42));
    }

    #[test]
    fn sparse_overlay_try_set_rejects_over_max() {
        let mut s: SparseOverlay<i32> = SparseOverlay::new();
        let result = s.try_set(NodeId::new(MAX_NODE_ID), 42);
        assert_eq!(result, Err(42));
        assert!(s.is_empty());
    }
}
