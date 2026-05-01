#![allow(dead_code)]
//! Core types for the TrueType hinting VM.
//!
//! All hinting arithmetic uses F26Dot6 fixed-point (26.6 format) where
//! 1 pixel = 64 units. This matches the TrueType specification exactly.

use std::ops::{Add, Neg, Sub};

/// 26.6 fixed-point number. Low 6 bits are fractional (1/64 pixel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct F26Dot6(pub i32);

impl F26Dot6 {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(64);
    pub const HALF: Self = Self(32);

    pub fn from_i32(v: i32) -> Self {
        Self(v)
    }

    pub fn from_font_units(fu: i32, ppem: u16, upem: u16) -> Self {
        if upem == 0 {
            return Self::ZERO;
        }
        Self(((fu as i64 * ppem as i64 * 64) / upem as i64) as i32)
    }

    pub fn from_pixels(px: f64) -> Self {
        Self((px * 64.0) as i32)
    }

    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 64.0
    }

    #[allow(dead_code)]
    pub fn to_i32(self) -> i32 {
        self.0
    }

    pub fn round(self) -> Self {
        Self((self.0 + 32) & !63)
    }

    pub fn floor(self) -> Self {
        Self(self.0 & !63)
    }

    pub fn ceiling(self) -> Self {
        Self((self.0 + 63) & !63)
    }

    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }

    #[allow(dead_code)]
    pub fn mul_div(self, b: i32, c: i32) -> Self {
        if c == 0 {
            return Self::ZERO;
        }
        Self(((self.0 as i64 * b as i64) / c as i64) as i32)
    }
}

impl Add for F26Dot6 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for F26Dot6 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl Neg for F26Dot6 {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

/// A point in 26.6 fixed-point pixel coordinates.
#[derive(Debug, Clone, Copy, Default)]
pub struct PointF26Dot6 {
    pub x: F26Dot6,
    pub y: F26Dot6,
}

/// F2Dot14 unit vector (used for projection/freedom vectors).
/// x and y are in 2.14 fixed-point: 1.0 = 0x4000.
pub const F2DOT14_ONE: i32 = 0x4000;

/// A zone of points (twilight zone 0 or glyph zone 1).
pub struct Zone {
    /// Original (unmodified) point positions in 26.6 pixel coords.
    pub original: Vec<PointF26Dot6>,
    /// Current (hinted) point positions.
    pub current: Vec<PointF26Dot6>,
    /// Whether each point has been touched in the x direction.
    pub touched_x: Vec<bool>,
    /// Whether each point has been touched in the y direction.
    pub touched_y: Vec<bool>,
    /// On-curve flags (from glyph data).
    pub on_curve: Vec<bool>,
    /// End-of-contour point indices.
    pub contour_ends: Vec<usize>,
}

impl Zone {
    pub fn new(num_points: usize) -> Self {
        Self {
            original: vec![PointF26Dot6::default(); num_points],
            current: vec![PointF26Dot6::default(); num_points],
            touched_x: vec![false; num_points],
            touched_y: vec![false; num_points],
            on_curve: vec![true; num_points],
            contour_ends: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.current.len()
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_empty()
    }
}

/// Rounding mode for the hinting VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundState {
    /// Round to grid (nearest integer pixel).
    RoundToGrid,
    /// Round to half-grid (nearest half pixel).
    RoundToHalfGrid,
    /// Round to double grid (nearest 2-pixel boundary).
    RoundToDoubleGrid,
    /// Round down to grid.
    RoundDownToGrid,
    /// Round up to grid.
    RoundUpToGrid,
    /// No rounding.
    RoundOff,
    /// Super round (custom period/phase/threshold).
    Super(i32), // packed period/phase/threshold
    /// Super round 45 degrees.
    Super45(i32),
}

/// Graphics state for the hinting VM.
pub struct GraphicsState {
    // Reference points
    pub rp0: usize,
    pub rp1: usize,
    pub rp2: usize,

    // Zone pointers (0 = twilight, 1 = glyph)
    pub zp0: usize,
    pub zp1: usize,
    pub zp2: usize,

    // Projection and freedom vectors (F2Dot14: 1.0 = 0x4000)
    pub projection_vector: (i32, i32),
    pub freedom_vector: (i32, i32),
    pub dual_projection_vector: (i32, i32),

    // Rounding
    pub round_state: RoundState,

    // Control value cut-in
    pub control_value_cut_in: F26Dot6,

    // Single width cut-in and value
    pub single_width_cut_in: F26Dot6,
    pub single_width_value: F26Dot6,

    // Minimum distance
    pub minimum_distance: F26Dot6,

    // Auto flip
    pub auto_flip: bool,

    // Delta base and shift
    pub delta_base: u16,
    pub delta_shift: u16,

    // Loop count
    pub loop_value: u32,

    // Instruction control
    pub instruct_control: u8,

    // Scan control
    pub scan_control: bool,
    pub scan_type: u16,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            rp0: 0,
            rp1: 0,
            rp2: 0,
            zp0: 1,
            zp1: 1,
            zp2: 1,
            projection_vector: (F2DOT14_ONE, 0), // x-axis
            freedom_vector: (F2DOT14_ONE, 0),    // x-axis
            dual_projection_vector: (F2DOT14_ONE, 0),
            round_state: RoundState::RoundToGrid,
            control_value_cut_in: F26Dot6(68), // 17/16 pixel
            single_width_cut_in: F26Dot6(0),
            single_width_value: F26Dot6(0),
            minimum_distance: F26Dot6(64), // 1 pixel
            auto_flip: true,
            delta_base: 9,
            delta_shift: 3,
            loop_value: 1,
            instruct_control: 0,
            scan_control: false,
            scan_type: 0,
        }
    }
}

impl Clone for GraphicsState {
    fn clone(&self) -> Self {
        Self {
            rp0: self.rp0,
            rp1: self.rp1,
            rp2: self.rp2,
            zp0: self.zp0,
            zp1: self.zp1,
            zp2: self.zp2,
            projection_vector: self.projection_vector,
            freedom_vector: self.freedom_vector,
            dual_projection_vector: self.dual_projection_vector,
            round_state: self.round_state,
            control_value_cut_in: self.control_value_cut_in,
            single_width_cut_in: self.single_width_cut_in,
            single_width_value: self.single_width_value,
            minimum_distance: self.minimum_distance,
            auto_flip: self.auto_flip,
            delta_base: self.delta_base,
            delta_shift: self.delta_shift,
            loop_value: self.loop_value,
            instruct_control: self.instruct_control,
            scan_control: self.scan_control,
            scan_type: self.scan_type,
        }
    }
}

/// Errors from the hinting VM.
#[derive(Debug)]
pub enum HintError {
    /// Instruction limit exceeded (infinite loop protection).
    InstructionLimit,
    /// Stack overflow or underflow.
    StackError,
    /// Invalid instruction opcode.
    InvalidInstruction(u8),
    /// Invalid point or zone reference.
    InvalidReference,
}

impl std::fmt::Display for HintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InstructionLimit => write!(f, "instruction limit exceeded"),
            Self::StackError => write!(f, "stack overflow or underflow"),
            Self::InvalidInstruction(op) => write!(f, "invalid instruction 0x{op:02X}"),
            Self::InvalidReference => write!(f, "invalid point or zone reference"),
        }
    }
}
