//! Shared geometry types used across the crate.
//!
//! These types represent spatial concepts in PDF user-space coordinates
//! (points, 1/72 inch, origin at bottom-left, Y increases upward).

use std::fmt;

/// Axis-aligned bounding box in PDF user-space coordinates.
///
/// Coordinates use the PDF convention: origin at bottom-left, Y increases
/// upward. `x_min`/`y_min` is the bottom-left corner, `x_max`/`y_max` is
/// the top-right corner.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    pub fn new(x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> Self {
        Self {
            x_min: x_min.min(x_max),
            y_min: y_min.min(y_max),
            x_max: x_min.max(x_max),
            y_max: y_min.max(y_max),
        }
    }

    /// Width of the bounding box (always non-negative).
    pub fn width(&self) -> f64 {
        self.x_max - self.x_min
    }

    /// Height of the bounding box (always non-negative).
    pub fn height(&self) -> f64 {
        self.y_max - self.y_min
    }

    /// Whether the point (x, y) lies inside or on the boundary of this box.
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.x_min && x <= self.x_max && y >= self.y_min && y <= self.y_max
    }

    /// Whether this box intersects (overlaps) another box.
    /// Touching edges count as intersecting.
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
    fn test_new_normalizes_coordinates() {
        let bbox = BoundingBox::new(100.0, 200.0, 50.0, 100.0);
        assert_eq!(bbox.x_min, 50.0);
        assert_eq!(bbox.y_min, 100.0);
        assert_eq!(bbox.x_max, 100.0);
        assert_eq!(bbox.y_max, 200.0);
    }

    #[test]
    fn test_width_height() {
        let bbox = BoundingBox::new(10.0, 20.0, 110.0, 70.0);
        assert!((bbox.width() - 100.0).abs() < f64::EPSILON);
        assert!((bbox.height() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_contains_point() {
        let bbox = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        assert!(bbox.contains_point(50.0, 50.0));
        assert!(bbox.contains_point(0.0, 0.0)); // on edge
        assert!(bbox.contains_point(100.0, 100.0)); // on edge
        assert!(!bbox.contains_point(-1.0, 50.0));
        assert!(!bbox.contains_point(50.0, 101.0));
    }

    #[test]
    fn test_intersects() {
        let a = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        let b = BoundingBox::new(50.0, 50.0, 150.0, 150.0);
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));

        let c = BoundingBox::new(200.0, 200.0, 300.0, 300.0);
        assert!(!a.intersects(&c));

        // touching edges count as intersecting
        let d = BoundingBox::new(100.0, 0.0, 200.0, 100.0);
        assert!(a.intersects(&d));
    }

    #[test]
    fn test_merge() {
        let a = BoundingBox::new(10.0, 20.0, 50.0, 60.0);
        let b = BoundingBox::new(30.0, 5.0, 80.0, 40.0);
        let merged = a.merge(&b);
        assert!((merged.x_min - 10.0).abs() < f64::EPSILON);
        assert!((merged.y_min - 5.0).abs() < f64::EPSILON);
        assert!((merged.x_max - 80.0).abs() < f64::EPSILON);
        assert!((merged.y_max - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_display() {
        let bbox = BoundingBox::new(1.0, 2.5, 100.123, 200.0);
        assert_eq!(format!("{}", bbox), "[1.00, 2.50, 100.12, 200.00]");
    }

    #[test]
    fn test_area() {
        let bbox = BoundingBox::new(0.0, 0.0, 10.0, 20.0);
        assert!((bbox.area() - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_intersection_area() {
        let a = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        let b = BoundingBox::new(50.0, 50.0, 150.0, 150.0);
        // overlap is 50x50 = 2500
        assert!((a.intersection_area(&b) - 2500.0).abs() < f64::EPSILON);

        let c = BoundingBox::new(200.0, 200.0, 300.0, 300.0);
        assert!((a.intersection_area(&c)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_iou() {
        let a = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        let b = BoundingBox::new(0.0, 0.0, 100.0, 100.0);
        // identical boxes: IoU = 1.0
        assert!((a.iou(&b) - 1.0).abs() < f64::EPSILON);

        let c = BoundingBox::new(50.0, 50.0, 150.0, 150.0);
        // intersection = 2500, union = 10000 + 10000 - 2500 = 17500
        let expected = 2500.0 / 17500.0;
        assert!((a.iou(&c) - expected).abs() < 1e-10);

        // no overlap
        let d = BoundingBox::new(200.0, 200.0, 300.0, 300.0);
        assert!((a.iou(&d)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_zero_area_box() {
        let bbox = BoundingBox::new(5.0, 5.0, 5.0, 5.0);
        assert!((bbox.width()).abs() < f64::EPSILON);
        assert!((bbox.height()).abs() < f64::EPSILON);
        assert!(bbox.contains_point(5.0, 5.0));
    }
}
