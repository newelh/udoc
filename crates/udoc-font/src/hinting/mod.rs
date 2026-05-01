//! TrueType hinting (grid-fitting) for glyph outlines.
//!
//! The hinting VM interprets TrueType bytecode instructions from the fpgm,
//! prep, and glyph programs to adjust point positions for optimal rendering
//! at a given pixel size.

pub(crate) mod types;
pub(crate) mod vm;

use types::F26Dot6;
pub use types::HintError;
use vm::Vm;

use super::ttf::{HintingLimits, OutlinePoint, RawGlyphData};

/// Result of hinting a glyph. Points are in pixel coordinates (f64).
pub struct HintedGlyph {
    /// Hinted outline points in pixel coordinates.
    pub points: Vec<OutlinePoint>,
    /// Contour end indices (unchanged from input).
    pub contour_ends: Vec<usize>,
    /// Hinted advance width in pixels.
    pub advance_width: f64,
}

/// Hinting state for a TrueType font. Caches fpgm results and CVT.
pub struct HintingState {
    vm: Vm,
    current_ppem: Option<u16>,
}

impl HintingState {
    /// Create hinting state from font tables. Executes fpgm once.
    pub fn new(
        fpgm: &[u8],
        prep: &[u8],
        cvt_font_units: &[i16],
        limits: &HintingLimits,
    ) -> Result<Self, HintError> {
        let max_stack = (limits.max_stack_elements as usize).clamp(256, 65536);
        let max_storage = (limits.max_storage as usize).min(65536);
        let max_functions = (limits.max_function_defs as usize).min(65536);
        let max_twilight = (limits.max_twilight_points as usize).min(16384);

        let mut vm = Vm::new(max_stack, max_storage, max_functions, max_twilight);
        vm.init_cvt(cvt_font_units);
        vm.set_prep_program(prep);

        // Execute font program (defines functions).
        if !fpgm.is_empty() {
            vm.init_font_program(fpgm)?;
        }

        Ok(Self {
            vm,
            current_ppem: None,
        })
    }

    /// Prepare for rendering at a specific ppem.
    pub fn prepare_size(&mut self, ppem: u16, units_per_em: u16) -> Result<(), HintError> {
        if self.current_ppem == Some(ppem) {
            return Ok(());
        }
        self.vm.prepare_size(ppem, units_per_em)?;
        self.current_ppem = Some(ppem);
        Ok(())
    }

    /// Hint a glyph's outline. Returns hinted points in pixel coordinates.
    pub fn hint_glyph(&mut self, raw: &RawGlyphData) -> Result<HintedGlyph, HintError> {
        let ppem = self.vm.current_ppem();
        let upem = self.vm.current_upem();

        // Scale outline points from font units to F26Dot6 pixel coords.
        let scaled_points: Vec<(F26Dot6, F26Dot6, bool)> = raw
            .points
            .iter()
            .map(|p| {
                let x = F26Dot6::from_font_units(p.x as i32, ppem, upem);
                let y = F26Dot6::from_font_units(p.y as i32, ppem, upem);
                (x, y, p.on_curve)
            })
            .collect();

        let aw = F26Dot6::from_font_units(raw.advance_width as i32, ppem, upem);
        let lsb = F26Dot6::from_font_units(raw.lsb as i32, ppem, upem);

        // Set up glyph zone.
        self.vm
            .setup_glyph_zone(&scaled_points, &raw.contour_ends, aw, lsb);

        // Execute per-glyph instructions.
        if !raw.instructions.is_empty() {
            let instructions = raw.instructions.clone();
            // If glyph instructions fail, return unhinted points rather than
            // failing the entire render. Many fonts have minor instruction issues.
            if let Err(e) = self.vm.execute(&instructions) {
                if std::env::var("UDOC_TT_HINT_DEBUG").is_ok() {
                    eprintln!("tt-hint glyph execute failed: {e}");
                }
                return self.extract_unhinted(raw, ppem, upem);
            }
        }

        self.extract_hinted(raw)
    }

    fn extract_hinted(&self, raw: &RawGlyphData) -> Result<HintedGlyph, HintError> {
        let n = raw.points.len();
        let mut points = Vec::with_capacity(n);
        for i in 0..n {
            if i < self.vm.glyph.current.len() {
                points.push(OutlinePoint {
                    x: self.vm.glyph.current[i].x.to_f64(),
                    y: self.vm.glyph.current[i].y.to_f64(),
                    on_curve: if i < self.vm.glyph.on_curve.len() {
                        self.vm.glyph.on_curve[i]
                    } else {
                        true
                    },
                });
            }
        }

        // Advance width from phantom point.
        let advance_width = if n < self.vm.glyph.current.len() {
            self.vm.glyph.current[n].x.to_f64()
        } else {
            0.0
        };

        Ok(HintedGlyph {
            points,
            contour_ends: raw.contour_ends.clone(),
            advance_width,
        })
    }

    fn extract_unhinted(
        &self,
        raw: &RawGlyphData,
        ppem: u16,
        upem: u16,
    ) -> Result<HintedGlyph, HintError> {
        let points: Vec<OutlinePoint> = raw
            .points
            .iter()
            .map(|p| OutlinePoint {
                x: F26Dot6::from_font_units(p.x as i32, ppem, upem).to_f64(),
                y: F26Dot6::from_font_units(p.y as i32, ppem, upem).to_f64(),
                on_curve: p.on_curve,
            })
            .collect();
        let advance_width = F26Dot6::from_font_units(raw.advance_width as i32, ppem, upem).to_f64();
        Ok(HintedGlyph {
            points,
            contour_ends: raw.contour_ends.clone(),
            advance_width,
        })
    }
}
