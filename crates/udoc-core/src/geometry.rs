//! Geometry types for document extraction.
//!
//! These types represent spatial concepts in page coordinates.
//! Coordinates use the convention: origin at bottom-left, Y increases upward,
//! units are points (1/72 inch).

use std::fmt;

/// Axis-aligned bounding box in page coordinates.
///
/// Coordinates are format-native: PDF uses y-up (origin at bottom-left),
/// OOXML formats have no geometric coordinates (bbox is `Option` on
/// non-PDF types). Hook consumers should be aware of the coordinate
/// system when processing bbox values.
///
/// For PDF specifically: origin at bottom-left, X increases rightward,
/// Y increases upward. Units are points (1/72 inch). `x_min`/`y_min`
/// is the bottom-left corner, `x_max`/`y_max` is the top-right corner.
/// Coordinates are normalized on construction so that min <= max on
/// both axes.
///
/// OCR tools (Tesseract, hOCR) typically use top-left origin with Y
/// increasing downward. To convert, given a known `page_height`:
/// `y_top_down = page_height - y_bottom_up`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct BoundingBox {
    /// Left edge (minimum X).
    pub x_min: f64,
    /// Bottom edge (minimum Y).
    pub y_min: f64,
    /// Right edge (maximum X).
    pub x_max: f64,
    /// Top edge (maximum Y).
    pub y_max: f64,
}

impl BoundingBox {
    /// Create a new bounding box. Coordinates are normalized so that
    /// min <= max on both axes.
    ///
    /// ```
    /// use udoc_core::geometry::BoundingBox;
    /// let bbox = BoundingBox::new(100.0, 200.0, 50.0, 100.0);
    /// // Coordinates are normalized: the larger value becomes max.
    /// assert_eq!(bbox.x_min, 50.0);
    /// assert_eq!(bbox.x_max, 100.0);
    /// ```
    pub fn new(x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> Self {
        Self {
            x_min: x_min.min(x_max),
            y_min: y_min.min(y_max),
            x_max: x_min.max(x_max),
            y_max: y_min.max(y_max),
        }
    }

    /// Width of the bounding box (always non-negative).
    ///
    /// ```
    /// use udoc_core::geometry::BoundingBox;
    /// let bbox = BoundingBox::new(10.0, 20.0, 110.0, 70.0);
    /// assert!((bbox.width() - 100.0).abs() < f64::EPSILON);
    /// ```
    pub fn width(&self) -> f64 {
        self.x_max - self.x_min
    }

    /// Height of the bounding box (always non-negative).
    ///
    /// ```
    /// use udoc_core::geometry::BoundingBox;
    /// let bbox = BoundingBox::new(10.0, 20.0, 110.0, 70.0);
    /// assert!((bbox.height() - 50.0).abs() < f64::EPSILON);
    /// ```
    pub fn height(&self) -> f64 {
        self.y_max - self.y_min
    }

    /// Whether the point (x, y) lies inside or on the boundary.
    ///
    /// ```
    /// use udoc_core::geometry::BoundingBox;
    /// let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
    /// assert!(bbox.contains_point(50.0, 50.0));
    /// assert!(!bbox.contains_point(-1.0, 50.0));
    /// ```
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.x_min && x <= self.x_max && y >= self.y_min && y <= self.y_max
    }

    /// Whether this box intersects (overlaps) another box.
    /// Touching edges count as intersecting.
    ///
    /// ```
    /// use udoc_core::geometry::BoundingBox;
    /// let a = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
    /// let b = BoundingBox::new(50.0, 50.0, 150.0, 150.0);
    /// assert!(a.intersects(&b));
    /// ```
    pub fn intersects(&self, other: &BoundingBox) -> bool {
        self.x_min <= other.x_max
            && self.x_max >= other.x_min
            && self.y_min <= other.y_max
            && self.y_max >= other.y_min
    }

    /// Area of the bounding box (width * height).
    pub fn area(&self) -> f64 {
        self.width() * self.height()
    }

    /// Area of intersection between two bounding boxes.
    /// Returns 0.0 if they don't overlap.
    pub fn intersection_area(&self, other: &BoundingBox) -> f64 {
        let x_overlap = (self.x_max.min(other.x_max) - self.x_min.max(other.x_min)).max(0.0);
        let y_overlap = (self.y_max.min(other.y_max) - self.y_min.max(other.y_min)).max(0.0);
        x_overlap * y_overlap
    }

    /// Intersection over Union (IoU) of two bounding boxes.
    /// Returns 0.0 if they don't overlap or both have zero area.
    pub fn iou(&self, other: &BoundingBox) -> f64 {
        let intersection = self.intersection_area(other);
        if intersection == 0.0 {
            return 0.0;
        }
        let union = self.area() + other.area() - intersection;
        if union <= 0.0 {
            return 0.0;
        }
        intersection / union
    }

    /// Return the smallest bounding box that contains both `self` and `other`.
    pub fn merge(&self, other: &BoundingBox) -> BoundingBox {
        BoundingBox {
            x_min: self.x_min.min(other.x_min),
            y_min: self.y_min.min(other.y_min),
            x_max: self.x_max.max(other.x_max),
            y_max: self.y_max.max(other.y_max),
        }
    }
}

impl fmt::Display for BoundingBox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{:.2}, {:.2}, {:.2}, {:.2}]",
            self.x_min, self.y_min, self.x_max, self.y_max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_normalizes_coordinates() {
        let bbox = BoundingBox::new(100.0, 200.0, 50.0, 100.0);
        assert_eq!(bbox.x_min, 50.0);
        assert_eq!(bbox.y_min, 100.0);
        assert_eq!(bbox.x_max, 100.0);
        assert_eq!(bbox.y_max, 200.0);
    }

    #[test]
    fn width_height() {
        let bbox = BoundingBox::new(10.0, 20.0, 110.0, 70.0);
        assert!((bbox.width() - 100.0).abs() < f64::EPSILON);
        assert!((bbox.height() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn contains_point() {
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        assert!(bbox.contains_point(50.0, 50.0));
        assert!(bbox.contains_point(0.0, 0.0));
        assert!(!bbox.contains_point(-1.0, 50.0));
    }

    #[test]
    fn intersects() {
        let a = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        let b = BoundingBox::new(50.0, 50.0, 150.0, 150.0);
        assert!(a.intersects(&b));

        let c = BoundingBox::new(200.0, 200.0, 300.0, 300.0);
        assert!(!a.intersects(&c));
    }

    #[test]
    fn merge_boxes() {
        let a = BoundingBox::new(10.0, 20.0, 50.0, 60.0);
        let b = BoundingBox::new(30.0, 5.0, 80.0, 40.0);
        let m = a.merge(&b);
        assert!((m.x_min - 10.0).abs() < f64::EPSILON);
        assert!((m.y_min - 5.0).abs() < f64::EPSILON);
        assert!((m.x_max - 80.0).abs() < f64::EPSILON);
        assert!((m.y_max - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn area_and_iou() {
        let a = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        let b = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        assert!((a.iou(&b) - 1.0).abs() < f64::EPSILON);

        let c = BoundingBox::new(200.0, 200.0, 300.0, 300.0);
        assert!(a.iou(&c).abs() < f64::EPSILON);
    }

    #[test]
    fn display() {
        let bbox = BoundingBox::new(1.0, 2.5, 100.123, 200.0);
        assert_eq!(format!("{bbox}"), "[1.00, 2.50, 100.12, 200.00]");
    }
}
