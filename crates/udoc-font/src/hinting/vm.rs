//! TrueType hinting virtual machine.
//!
//! Implements the TrueType instruction set for grid-fitting glyph outlines.
//! The VM operates on 26.6 fixed-point coordinates and manipulates glyph points
//! along projection and freedom vectors to align them to the pixel grid.

use super::types::*;

const MAX_INSTRUCTIONS: u64 = 1_000_000;
const MAX_CALL_DEPTH: u32 = 64;

pub struct Vm {
    stack: Vec<i32>,
    pub graphics_state: GraphicsState,
    default_graphics_state: GraphicsState,
    functions: Vec<Vec<u8>>,
    storage: Vec<i32>,
    pub cvt: Vec<F26Dot6>,
    cvt_font_units: Vec<i16>,
    pub twilight: Zone,
    pub glyph: Zone,
    ppem: u16,
    upem: u16,
    scale: F26Dot6,
    instruction_count: u64,
    max_stack_depth: usize,
    call_depth: u32,
    // Prep program bytecode, saved so we can re-run on size changes.
    prep_program: Vec<u8>,
}

impl Vm {
    pub fn new(
        max_stack: usize,
        max_storage: usize,
        max_functions: usize,
        max_twilight: usize,
    ) -> Self {
        Self {
            stack: Vec::with_capacity(max_stack.min(4096)),
            graphics_state: GraphicsState::default(),
            default_graphics_state: GraphicsState::default(),
            functions: vec![Vec::new(); max_functions],
            storage: vec![0; max_storage],
            cvt: Vec::new(),
            cvt_font_units: Vec::new(),
            twilight: Zone::new(max_twilight),
            glyph: Zone::new(0),
            ppem: 0,
            upem: 1000,
            scale: F26Dot6::ZERO,
            instruction_count: 0,
            max_stack_depth: max_stack,
            call_depth: 0,
            prep_program: Vec::new(),
        }
    }

    // -- Stack helpers --

    fn push(&mut self, v: i32) -> Result<(), HintError> {
        if self.stack.len() >= self.max_stack_depth {
            return Err(HintError::StackError);
        }
        self.stack.push(v);
        Ok(())
    }

    fn pop(&mut self) -> Result<i32, HintError> {
        self.stack.pop().ok_or(HintError::StackError)
    }

    fn peek(&self) -> Result<i32, HintError> {
        self.stack.last().copied().ok_or(HintError::StackError)
    }

    // -- Zone helpers --

    fn get_zone(&self, zp: usize) -> &Zone {
        if zp == 0 {
            &self.twilight
        } else {
            &self.glyph
        }
    }

    fn get_zone_mut(&mut self, zp: usize) -> &mut Zone {
        if zp == 0 {
            &mut self.twilight
        } else {
            &mut self.glyph
        }
    }

    fn check_point(&self, zp: usize, idx: usize) -> Result<(), HintError> {
        let zone = self.get_zone(zp);
        if idx >= zone.len() {
            return Err(HintError::InvalidReference);
        }
        Ok(())
    }

    // -- Vector/projection helpers --

    /// Dot product of (px, py) with the projection vector, result in 26.6.
    fn project_xy(&self, px: F26Dot6, py: F26Dot6) -> F26Dot6 {
        let (vx, vy) = self.graphics_state.projection_vector;
        // Vectors are F2Dot14 (1.0 = 0x4000). Multiply and shift by 14.
        let val = (px.0 as i64 * vx as i64 + py.0 as i64 * vy as i64) >> 14;
        F26Dot6(val as i32)
    }

    /// Project a point's current position onto the projection vector.
    fn project(&self, zone_idx: usize, point_idx: usize) -> F26Dot6 {
        let zone = self.get_zone(zone_idx);
        let pt = zone.current[point_idx];
        self.project_xy(pt.x, pt.y)
    }

    /// Dual-project using original positions (for distance measurement pre-hinting).
    fn dual_project(&self, zone_idx: usize, point_idx: usize) -> F26Dot6 {
        let zone = self.get_zone(zone_idx);
        let pt = zone.original[point_idx];
        let (vx, vy) = self.graphics_state.dual_projection_vector;
        let val = (pt.x.0 as i64 * vx as i64 + pt.y.0 as i64 * vy as i64) >> 14;
        F26Dot6(val as i32)
    }

    /// Move a point along the freedom vector by `distance` (in 26.6, projected units).
    fn move_point(&mut self, zone_idx: usize, point_idx: usize, distance: F26Dot6) {
        let (fx, fy) = self.graphics_state.freedom_vector;
        let (px, py) = self.graphics_state.projection_vector;

        // Compute freedom dot projection to convert projected distance to movement.
        let dot = (fx as i64 * px as i64 + fy as i64 * py as i64) >> 14;
        if dot == 0 {
            return;
        }

        let zone = self.get_zone_mut(zone_idx);
        let dx = ((distance.0 as i64 * fx as i64) << 14) / (dot * F2DOT14_ONE as i64);
        let dy = ((distance.0 as i64 * fy as i64) << 14) / (dot * F2DOT14_ONE as i64);

        zone.current[point_idx].x.0 += dx as i32;
        zone.current[point_idx].y.0 += dy as i32;
    }

    /// Touch a point on whichever axis the freedom vector favors.
    fn touch_point(&mut self, zone_idx: usize, point_idx: usize) {
        let (fx, fy) = self.graphics_state.freedom_vector;
        let zone = self.get_zone_mut(zone_idx);
        if fx.abs() > fy.abs() || (fx.abs() == fy.abs() && fx != 0) {
            zone.touched_x[point_idx] = true;
        }
        if fy.abs() > fx.abs() || (fx.abs() == fy.abs() && fy != 0) {
            zone.touched_y[point_idx] = true;
        }
    }

    fn round_value(&self, value: F26Dot6) -> F26Dot6 {
        if value.0 == 0 {
            return F26Dot6::ZERO;
        }
        let sign = if value.0 < 0 { -1i32 } else { 1 };
        let abs_val = value.abs();

        let rounded = match self.graphics_state.round_state {
            RoundState::RoundToGrid => abs_val.round(),
            RoundState::RoundToHalfGrid => {
                let f = abs_val.floor();
                F26Dot6(f.0 + 32)
            }
            RoundState::RoundToDoubleGrid => {
                // Round to nearest 0.5 pixel (32 units)
                F26Dot6((abs_val.0 + 16) & !31)
            }
            RoundState::RoundDownToGrid => abs_val.floor(),
            RoundState::RoundUpToGrid => {
                if abs_val.0 & 63 == 0 {
                    abs_val
                } else {
                    abs_val.ceiling()
                }
            }
            RoundState::RoundOff => abs_val,
            RoundState::Super(packed) | RoundState::Super45(packed) => self.super_round(
                abs_val,
                packed,
                matches!(self.graphics_state.round_state, RoundState::Super45(_)),
            ),
        };

        // Rounded value must be at least 0 (but sign is restored)
        if rounded.0 == 0 {
            return F26Dot6::ZERO;
        }
        F26Dot6(rounded.0 * sign)
    }

    fn super_round(&self, value: F26Dot6, packed: i32, is_45: bool) -> F26Dot6 {
        // SROUND/S45ROUND decomposition of the packed byte:
        //   bits 7-6: period selector (0=1/2px, 1=1px, 2=2px, 3=reserved)
        //   bits 5-4: phase selector (0=0, 1=1/4, 2=1/2, 3=3/4 of period)
        //   bits 3-0: threshold selector
        let period_sel = (packed >> 6) & 0x3;
        let phase_sel = (packed >> 4) & 0x3;
        let thresh_sel = packed & 0xF;

        let base = if is_45 { 46 } else { 64 }; // sqrt(2)/2 * 64 ~ 46 for 45deg

        let period = match period_sel {
            0 => base / 2,
            1 => base,
            2 => base * 2,
            _ => base, // reserved, treat as 1px
        };

        let phase = match phase_sel {
            0 => 0,
            1 => period / 4,
            2 => period / 2,
            3 => period * 3 / 4,
            _ => 0,
        };

        let threshold = if thresh_sel == 0 {
            period - 1
        } else {
            ((thresh_sel - 4) * period + period / 2) / 8
        };

        // Round: find nearest value = phase + N*period, where N*period >= value - phase - threshold
        let val = value.0 - phase;
        if val >= 0 {
            let n = (val + threshold) / period;
            F26Dot6(n * period + phase)
        } else {
            let n = -(((-val) + threshold) / period);
            let result = n * period + phase;
            F26Dot6(if result < 0 { result + period } else { result })
        }
    }

    // -- Public entry points --

    /// Run the font program (fpgm). Typically defines functions via FDEF/ENDF.
    pub fn init_font_program(&mut self, fpgm: &[u8]) -> Result<(), HintError> {
        self.instruction_count = 0;
        self.graphics_state = GraphicsState::default();
        self.execute(fpgm)?;
        // After fpgm, snapshot as default graphics state
        self.default_graphics_state = self.graphics_state.clone();
        Ok(())
    }

    /// Store raw CVT font units (from the `cvt ` table).
    pub fn init_cvt(&mut self, cvt_fu: &[i16]) {
        self.cvt_font_units = cvt_fu.to_vec();
        self.cvt = vec![F26Dot6::ZERO; cvt_fu.len()];
    }

    /// Set the prep program bytecode (to re-run on size changes).
    pub fn set_prep_program(&mut self, prep: &[u8]) {
        self.prep_program = prep.to_vec();
    }

    pub fn current_ppem(&self) -> u16 {
        self.ppem
    }

    pub fn current_upem(&self) -> u16 {
        self.upem
    }

    /// Prepare for a new pixel size: scale CVT values and run the prep program.
    pub fn prepare_size(&mut self, ppem: u16, upem: u16) -> Result<(), HintError> {
        self.ppem = ppem;
        self.upem = upem;
        self.scale = if upem > 0 {
            F26Dot6((ppem as i32 * 64) / upem as i32)
        } else {
            F26Dot6::ONE
        };

        // Scale CVT from font units to 26.6
        for (i, &fu) in self.cvt_font_units.iter().enumerate() {
            if i < self.cvt.len() {
                self.cvt[i] = F26Dot6::from_font_units(fu as i32, ppem, upem);
            }
        }

        // Reset graphics state to post-fpgm defaults and run prep. If prep
        // trips on an opcode we don't implement we still proceed with the
        // CVT we already scaled (FreeType behaves the same way, treating
        // prep as best-effort for non-ClearType rendering targets).
        self.graphics_state = self.default_graphics_state.clone();
        self.instruction_count = 0;
        self.stack.clear();

        let prep = self.prep_program.clone();
        if !prep.is_empty() {
            if let Err(e) = self.execute(&prep) {
                if std::env::var("UDOC_TT_HINT_DEBUG").is_ok() {
                    eprintln!("tt-hint prep execute failed: {e}");
                }
            }
        }
        // Save post-prep state as default for per-glyph runs
        self.default_graphics_state = self.graphics_state.clone();
        Ok(())
    }

    /// Set up the glyph zone (zone 1) for hinting a specific glyph.
    pub fn setup_glyph_zone(
        &mut self,
        points: &[(F26Dot6, F26Dot6, bool)], // (x, y, on_curve)
        contour_ends: &[usize],
        _advance_width: F26Dot6,
        _lsb: F26Dot6,
    ) {
        let n = points.len();
        // Phantom points: 4 extra (origin, advance width, top, bottom)
        let total = n + 4;
        self.glyph = Zone::new(total);
        for (i, &(x, y, on_curve)) in points.iter().enumerate() {
            self.glyph.original[i] = PointF26Dot6 { x, y };
            self.glyph.current[i] = PointF26Dot6 { x, y };
            self.glyph.on_curve[i] = on_curve;
        }
        // Phantom points at indices n.n+3 are initialized to zero/defaults.
        // The caller can set them if needed. For now leave as (0,0).
        self.glyph.contour_ends = contour_ends.to_vec();

        // Reset per-glyph state
        self.graphics_state = self.default_graphics_state.clone();
        self.stack.clear();
        self.instruction_count = 0;
    }

    // -- Main dispatch loop --

    pub fn execute(&mut self, instructions: &[u8]) -> Result<(), HintError> {
        let len = instructions.len();
        let mut ip: usize = 0;

        while ip < len {
            self.instruction_count += 1;
            if self.instruction_count > MAX_INSTRUCTIONS {
                return Err(HintError::InstructionLimit);
            }

            let opcode = instructions[ip];
            ip += 1;

            match opcode {
                // -- Push instructions --
                0x40 => {
                    // NPUSHB: count byte, then count bytes pushed as unsigned
                    if ip >= len {
                        return Err(HintError::InvalidInstruction(opcode));
                    }
                    let n = instructions[ip] as usize;
                    ip += 1;
                    if ip + n > len {
                        return Err(HintError::InvalidInstruction(opcode));
                    }
                    for i in 0..n {
                        self.push(instructions[ip + i] as i32)?;
                    }
                    ip += n;
                }
                0x41 => {
                    // NPUSHW: count byte, then count words (2 bytes each, signed)
                    if ip >= len {
                        return Err(HintError::InvalidInstruction(opcode));
                    }
                    let n = instructions[ip] as usize;
                    ip += 1;
                    if ip + n * 2 > len {
                        return Err(HintError::InvalidInstruction(opcode));
                    }
                    for i in 0..n {
                        let hi = instructions[ip + i * 2];
                        let lo = instructions[ip + i * 2 + 1];
                        let word = ((hi as u16) << 8) | lo as u16;
                        self.push(word as i16 as i32)?;
                    }
                    ip += n * 2;
                }
                0xB0..=0xB7 => {
                    // PUSHB[n]: push n+1 bytes
                    let n = (opcode - 0xB0 + 1) as usize;
                    if ip + n > len {
                        return Err(HintError::InvalidInstruction(opcode));
                    }
                    for i in 0..n {
                        self.push(instructions[ip + i] as i32)?;
                    }
                    ip += n;
                }
                0xB8..=0xBF => {
                    // PUSHW[n]: push n+1 signed words
                    let n = (opcode - 0xB8 + 1) as usize;
                    if ip + n * 2 > len {
                        return Err(HintError::InvalidInstruction(opcode));
                    }
                    for i in 0..n {
                        let hi = instructions[ip + i * 2];
                        let lo = instructions[ip + i * 2 + 1];
                        let word = ((hi as u16) << 8) | lo as u16;
                        self.push(word as i16 as i32)?;
                    }
                    ip += n * 2;
                }

                // -- Stack manipulation --
                0x20 => {
                    // DUP
                    let v = self.peek()?;
                    self.push(v)?;
                }
                0x21 => {
                    // POP
                    self.pop()?;
                }
                0x22 => {
                    // CLEAR
                    self.stack.clear();
                }
                0x23 => {
                    // SWAP
                    let a = self.pop()?;
                    let b = self.pop()?;
                    self.push(a)?;
                    self.push(b)?;
                }
                0x24 => {
                    // DEPTH
                    let d = self.stack.len() as i32;
                    self.push(d)?;
                }
                0x25 => {
                    // CINDEX: copy indexed element to top (1-based)
                    let idx = self.pop()? as usize;
                    if idx == 0 || idx > self.stack.len() {
                        return Err(HintError::StackError);
                    }
                    let val = self.stack[self.stack.len() - idx];
                    self.push(val)?;
                }
                0x26 => {
                    // MINDEX: move indexed element to top (1-based), shift rest down
                    let idx = self.pop()? as usize;
                    if idx == 0 || idx > self.stack.len() {
                        return Err(HintError::StackError);
                    }
                    let pos = self.stack.len() - idx;
                    let val = self.stack.remove(pos);
                    self.stack.push(val);
                }
                0x8A => {
                    // ROLL: rotate top 3 elements (a b c -> b c a)
                    if self.stack.len() < 3 {
                        return Err(HintError::StackError);
                    }
                    let slen = self.stack.len();
                    let a = self.stack[slen - 3];
                    self.stack[slen - 3] = self.stack[slen - 2];
                    self.stack[slen - 2] = self.stack[slen - 1];
                    self.stack[slen - 1] = a;
                }

                0x60 => {
                    // ADD
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(a.wrapping_add(b))?;
                }
                0x61 => {
                    // SUB
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(a.wrapping_sub(b))?;
                }
                0x62 => {
                    // DIV: 26.6 / 26.6 -> 26.6
                    let b = self.pop()?;
                    let a = self.pop()?;
                    if b == 0 {
                        self.push(0)?;
                    } else {
                        self.push(((a as i64 * 64) / b as i64) as i32)?;
                    }
                }
                0x63 => {
                    // MUL: 26.6 * 26.6 >> 6 -> 26.6
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(((a as i64 * b as i64) >> 6) as i32)?;
                }
                0x64 => {
                    // ABS
                    let a = self.pop()?;
                    self.push(a.abs())?;
                }
                0x65 => {
                    // NEG
                    let a = self.pop()?;
                    self.push(-a)?;
                }
                0x66 => {
                    // FLOOR
                    let a = self.pop()?;
                    self.push(a & !63)?;
                }
                0x67 => {
                    // CEILING
                    let a = self.pop()?;
                    self.push((a + 63) & !63)?;
                }
                0x8B => {
                    // MAX
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(a.max(b))?;
                }
                0x8C => {
                    // MIN
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(a.min(b))?;
                }

                // -- ROUND / NROUND (engine compensation = 0 in FT) --
                0x68..=0x6B => {
                    // ROUND[ab]: pop value, round per current RoundState, push back.
                    // The low two bits select a device/color compensation class
                    // (black, gray, white, other). FreeType always uses 0 for
                    // all four, so we ignore the selector.
                    let v = self.pop()?;
                    let r = self.round_value(F26Dot6(v));
                    self.push(r.0)?;
                }
                0x6C..=0x6F => {
                    // NROUND[ab]: apply engine compensation only (no rounding).
                    // With compensation = 0 this is a no-op; we still pop and
                    // push to keep the stack well-formed.
                    let v = self.pop()?;
                    self.push(v)?;
                }

                // -- Deprecated / reserved opcodes (best-effort) --
                0x7E => {
                    // SANGW: deprecated, pops 1 argument and ignores it.
                    self.pop()?;
                }
                0x7F => {
                    // AA: deprecated, pops 1 argument and ignores it.
                    self.pop()?;
                }
                0x83 | 0x84 | 0x8F | 0x90 => {
                    // Unassigned in the TT spec; treat as no-ops so we don't
                    // abort prep on defensive/garbage bytes.
                }
                0x91 => {
                    // GETVARIATION (GX variation fonts). We don't support
                    // variations; FreeType returns zero for each axis. We
                    // don't know the axis count, so push nothing and carry on.
                    // Downstream code is very unlikely to rely on this in the
                    // fonts we care about (static Calibri/Times/DejaVu/etc).
                }
                0x92 => {
                    // GETDATA: FT returns the constant 17. Match that.
                    self.push(17)?;
                }

                // -- Logic / Comparison --
                0x50 => {
                    // LT
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a < b { 1 } else { 0 })?;
                }
                0x51 => {
                    // LTEQ
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a <= b { 1 } else { 0 })?;
                }
                0x52 => {
                    // GT
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a > b { 1 } else { 0 })?;
                }
                0x53 => {
                    // GTEQ
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a >= b { 1 } else { 0 })?;
                }
                0x54 => {
                    // EQ
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a == b { 1 } else { 0 })?;
                }
                0x55 => {
                    // NEQ
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a != b { 1 } else { 0 })?;
                }
                0x56 => {
                    // ODD: round value, check if pixel value is odd
                    let a = self.pop()?;
                    let rounded = self.round_value(F26Dot6(a));
                    let pixels = ((rounded.0 + 32) >> 6) & 1;
                    self.push(if pixels != 0 { 1 } else { 0 })?;
                }
                0x57 => {
                    // EVEN: round value, check if pixel value is even
                    let a = self.pop()?;
                    let rounded = self.round_value(F26Dot6(a));
                    let pixels = ((rounded.0 + 32) >> 6) & 1;
                    self.push(if pixels == 0 { 1 } else { 0 })?;
                }
                0x5A => {
                    // AND
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a != 0 && b != 0 { 1 } else { 0 })?;
                }
                0x5B => {
                    // OR
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if a != 0 || b != 0 { 1 } else { 0 })?;
                }
                0x5C => {
                    // NOT
                    let a = self.pop()?;
                    self.push(if a == 0 { 1 } else { 0 })?;
                }

                // -- Control flow --
                0x58 => {
                    // IF
                    let cond = self.pop()?;
                    if cond == 0 {
                        // Skip to matching ELSE or EIF
                        ip = self.skip_to_else_or_eif(instructions, ip)?;
                    }
                    // If true, just continue; the ELSE/EIF will be handled when encountered.
                }
                0x1B => {
                    // ELSE: if we're executing (came from a true IF), skip to EIF
                    ip = self.skip_to_eif(instructions, ip)?;
                }
                0x59 => {
                    // EIF: no-op, just a marker
                }
                0x1C => {
                    // JMPR: relative jump
                    let offset = self.pop()?;
                    ip = self.jump_relative(ip, offset)?;
                }
                0x78 => {
                    // JROT: jump relative on true
                    let cond = self.pop()?;
                    let offset = self.pop()?;
                    if cond != 0 {
                        ip = self.jump_relative(ip, offset)?;
                    }
                }
                0x79 => {
                    // JROF: jump relative on false
                    let cond = self.pop()?;
                    let offset = self.pop()?;
                    if cond == 0 {
                        ip = self.jump_relative(ip, offset)?;
                    }
                }
                0x2C => {
                    // FDEF: begin function definition
                    let func_num = self.pop()? as usize;
                    // Scan forward to find matching ENDF, record the bytecode
                    let start = ip;
                    let mut depth = 1u32;
                    while ip < len {
                        match instructions[ip] {
                            0x2C => {
                                depth += 1;
                                ip += 1;
                            }
                            0x2D => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                ip += 1;
                            }
                            // Skip inline data for push instructions
                            0x40 => {
                                ip += 1;
                                if ip < len {
                                    ip += instructions[ip] as usize + 1;
                                }
                            }
                            0x41 => {
                                ip += 1;
                                if ip < len {
                                    ip += instructions[ip] as usize * 2 + 1;
                                }
                            }
                            0xB0..=0xB7 => {
                                ip += (instructions[ip] - 0xB0 + 1) as usize + 1;
                            }
                            0xB8..=0xBF => {
                                ip += (instructions[ip] - 0xB8 + 1) as usize * 2 + 1;
                            }
                            _ => {
                                ip += 1;
                            }
                        }
                    }
                    // ip now points to ENDF
                    let body = instructions[start..ip].to_vec();
                    if func_num < self.functions.len() {
                        self.functions[func_num] = body;
                    }
                    ip += 1; // skip ENDF
                }
                0x2D => {
                    // ENDF: end of function body. Return from CALL.
                    // Handled by the call mechanism; if we hit this in the main loop
                    // it means we're in a top-level FDEF scan that already moved past,
                    // or a function returned normally.
                    return Ok(());
                }
                0x2B => {
                    // CALL
                    let func_num = self.pop()? as usize;
                    if func_num >= self.functions.len() {
                        return Err(HintError::InvalidReference);
                    }
                    self.call_depth += 1;
                    if self.call_depth > MAX_CALL_DEPTH {
                        self.call_depth -= 1;
                        return Err(HintError::InstructionLimit);
                    }
                    let body = self.functions[func_num].clone();
                    let result = self.execute(&body);
                    self.call_depth -= 1;
                    result?;
                }
                0x2A => {
                    // LOOPCALL
                    let func_num = self.pop()? as usize;
                    let count = self.pop()?;
                    if func_num >= self.functions.len() {
                        return Err(HintError::InvalidReference);
                    }
                    self.call_depth += 1;
                    if self.call_depth > MAX_CALL_DEPTH {
                        self.call_depth -= 1;
                        return Err(HintError::InstructionLimit);
                    }
                    let body = self.functions[func_num].clone();
                    for _ in 0..count {
                        let result = self.execute(&body);
                        result?;
                    }
                    self.call_depth -= 1;
                }

                // -- Storage / CVT --
                0x42 => {
                    // WS: write storage
                    let val = self.pop()?;
                    let idx = self.pop()? as usize;
                    if idx < self.storage.len() {
                        self.storage[idx] = val;
                    }
                }
                0x43 => {
                    // RS: read storage
                    let idx = self.pop()? as usize;
                    let val = if idx < self.storage.len() {
                        self.storage[idx]
                    } else {
                        0
                    };
                    self.push(val)?;
                }
                0x44 => {
                    // WCVTP: write CVT in pixel units (26.6)
                    let val = self.pop()?;
                    let idx = self.pop()? as usize;
                    if idx < self.cvt.len() {
                        self.cvt[idx] = F26Dot6(val);
                    }
                }
                0x45 => {
                    // RCVT: read CVT
                    let idx = self.pop()? as usize;
                    let val = if idx < self.cvt.len() {
                        self.cvt[idx].0
                    } else {
                        0
                    };
                    self.push(val)?;
                }
                0x70 => {
                    // WCVTF: write CVT in font units (convert to 26.6)
                    let val = self.pop()?;
                    let idx = self.pop()? as usize;
                    if idx < self.cvt.len() {
                        self.cvt[idx] = F26Dot6::from_font_units(val, self.ppem, self.upem);
                    }
                }

                // -- Vector setting --
                0x00 | 0x01 => {
                    // SVTCA[a]: set both projection and freedom to axis
                    // 0x00 = Y axis, 0x01 = X axis
                    let vec = if opcode == 0x01 {
                        (F2DOT14_ONE, 0)
                    } else {
                        (0, F2DOT14_ONE)
                    };
                    self.graphics_state.projection_vector = vec;
                    self.graphics_state.freedom_vector = vec;
                    self.graphics_state.dual_projection_vector = vec;
                }
                0x02 | 0x03 => {
                    // SPVTCA[a]: set projection vector to axis
                    let vec = if opcode == 0x03 {
                        (F2DOT14_ONE, 0)
                    } else {
                        (0, F2DOT14_ONE)
                    };
                    self.graphics_state.projection_vector = vec;
                    self.graphics_state.dual_projection_vector = vec;
                }
                0x04 | 0x05 => {
                    // SFVTCA[a]: set freedom vector to axis
                    let vec = if opcode == 0x05 {
                        (F2DOT14_ONE, 0)
                    } else {
                        (0, F2DOT14_ONE)
                    };
                    self.graphics_state.freedom_vector = vec;
                }
                0x06 | 0x07 => {
                    // SPVTL[a]: set projection vector to line
                    let p2 = self.pop()? as usize;
                    let p1 = self.pop()? as usize;
                    let perpendicular = opcode & 1 != 0;
                    let vec = self.compute_line_vector(
                        self.graphics_state.zp2,
                        p2,
                        self.graphics_state.zp1,
                        p1,
                        perpendicular,
                    );
                    self.graphics_state.projection_vector = vec;
                    self.graphics_state.dual_projection_vector = vec;
                }
                0x08 | 0x09 => {
                    // SFVTL[a]: set freedom vector to line
                    let p2 = self.pop()? as usize;
                    let p1 = self.pop()? as usize;
                    let perpendicular = opcode & 1 != 0;
                    let vec = self.compute_line_vector(
                        self.graphics_state.zp2,
                        p2,
                        self.graphics_state.zp1,
                        p1,
                        perpendicular,
                    );
                    self.graphics_state.freedom_vector = vec;
                }
                0x86 | 0x87 => {
                    // SDPVTL[a]: Set Dual Projection Vector To Line.
                    // FT derives dualVector from ORIGINAL positions and
                    // projVector from CURRENT positions, then normalizes
                    // both. The low bit flips the vector 90 degrees
                    // counter-clockwise.
                    let p2 = self.pop()? as usize;
                    let p1 = self.pop()? as usize;
                    let perpendicular = opcode & 1 != 0;

                    self.check_point(self.graphics_state.zp1, p2)?;
                    self.check_point(self.graphics_state.zp2, p1)?;

                    let dual = self.compute_line_vector_original(
                        self.graphics_state.zp1,
                        p2,
                        self.graphics_state.zp2,
                        p1,
                        perpendicular,
                    );
                    let proj = self.compute_line_vector(
                        self.graphics_state.zp1,
                        p2,
                        self.graphics_state.zp2,
                        p1,
                        perpendicular,
                    );
                    self.graphics_state.dual_projection_vector = dual;
                    self.graphics_state.projection_vector = proj;
                }
                0x0A => {
                    // SPVFS: set projection vector from stack (F2Dot14 y, x)
                    let y = self.pop()?;
                    let x = self.pop()?;
                    self.graphics_state.projection_vector = (x, y);
                    self.graphics_state.dual_projection_vector = (x, y);
                }
                0x0B => {
                    // SFVFS: set freedom vector from stack
                    let y = self.pop()?;
                    let x = self.pop()?;
                    self.graphics_state.freedom_vector = (x, y);
                }
                0x0C => {
                    // GPV: get projection vector
                    let (x, y) = self.graphics_state.projection_vector;
                    self.push(x)?;
                    self.push(y)?;
                }
                0x0D => {
                    // GFV: get freedom vector
                    let (x, y) = self.graphics_state.freedom_vector;
                    self.push(x)?;
                    self.push(y)?;
                }

                // -- Zone / Reference points --
                0x10 => {
                    // SRP0
                    self.graphics_state.rp0 = self.pop()? as usize;
                }
                0x11 => {
                    // SRP1
                    self.graphics_state.rp1 = self.pop()? as usize;
                }
                0x12 => {
                    // SRP2
                    self.graphics_state.rp2 = self.pop()? as usize;
                }
                0x13 => {
                    // SZP0
                    let z = self.pop()? as usize;
                    if z > 1 {
                        return Err(HintError::InvalidReference);
                    }
                    self.graphics_state.zp0 = z;
                }
                0x14 => {
                    // SZP1
                    let z = self.pop()? as usize;
                    if z > 1 {
                        return Err(HintError::InvalidReference);
                    }
                    self.graphics_state.zp1 = z;
                }
                0x15 => {
                    // SZP2
                    let z = self.pop()? as usize;
                    if z > 1 {
                        return Err(HintError::InvalidReference);
                    }
                    self.graphics_state.zp2 = z;
                }
                0x16 => {
                    // SZPS: set all zone pointers
                    let z = self.pop()? as usize;
                    if z > 1 {
                        return Err(HintError::InvalidReference);
                    }
                    self.graphics_state.zp0 = z;
                    self.graphics_state.zp1 = z;
                    self.graphics_state.zp2 = z;
                }

                // -- Rounding state --
                0x18 => {
                    self.graphics_state.round_state = RoundState::RoundToGrid;
                }
                0x19 => {
                    self.graphics_state.round_state = RoundState::RoundToHalfGrid;
                }
                0x3D => {
                    self.graphics_state.round_state = RoundState::RoundToDoubleGrid;
                }
                0x7C => {
                    self.graphics_state.round_state = RoundState::RoundUpToGrid;
                }
                0x7D => {
                    self.graphics_state.round_state = RoundState::RoundDownToGrid;
                }
                0x7A => {
                    self.graphics_state.round_state = RoundState::RoundOff;
                }
                0x76 => {
                    // SROUND
                    let n = self.pop()?;
                    self.graphics_state.round_state = RoundState::Super(n);
                }
                0x77 => {
                    // S45ROUND
                    let n = self.pop()?;
                    self.graphics_state.round_state = RoundState::Super45(n);
                }
                0x5E => {
                    // SDB: set delta base
                    let n = self.pop()?;
                    self.graphics_state.delta_base = n as u16;
                }
                0x5F => {
                    // SDS: set delta shift (0..=6)
                    let n = self.pop()?;
                    if (0..=6).contains(&n) {
                        self.graphics_state.delta_shift = n as u16;
                    }
                    // FT would raise Bad_Argument when pedantic; we silently
                    // ignore out-of-range values so buggy prep programs still
                    // run to completion.
                }

                // -- Loop, minimum distance, cut-in, single width --
                0x17 => {
                    // SLOOP
                    let n = self.pop()?;
                    self.graphics_state.loop_value = n.max(1) as u32;
                }
                0x1A => {
                    // SMD: set minimum distance
                    let d = self.pop()?;
                    self.graphics_state.minimum_distance = F26Dot6(d);
                }
                0x1D => {
                    // SCVTCI: set control value cut-in
                    let d = self.pop()?;
                    self.graphics_state.control_value_cut_in = F26Dot6(d);
                }
                0x1E => {
                    // SSWCI: set single width cut-in
                    let d = self.pop()?;
                    self.graphics_state.single_width_cut_in = F26Dot6(d);
                }
                0x1F => {
                    // SSW: set single width value (font units on stack, store as 26.6)
                    let val = self.pop()?;
                    self.graphics_state.single_width_value =
                        F26Dot6::from_font_units(val, self.ppem, self.upem);
                }

                0x2E | 0x2F => {
                    // MDAP[a]: move direct absolute point. bit0 = round
                    let do_round = opcode & 1 != 0;
                    let point = self.pop()? as usize;
                    self.check_point(self.graphics_state.zp0, point)?;

                    if do_round {
                        let cur_proj = self.project(self.graphics_state.zp0, point);
                        let rounded = self.round_value(cur_proj);
                        let delta = rounded - cur_proj;
                        self.move_point(self.graphics_state.zp0, point, delta);
                    }
                    self.touch_point(self.graphics_state.zp0, point);
                    self.graphics_state.rp0 = point;
                    self.graphics_state.rp1 = point;
                }

                0x3E | 0x3F => {
                    // MIAP[a]: move indirect absolute point. bit0 = round
                    let do_round = opcode & 1 != 0;
                    let cvt_idx = self.pop()? as usize;
                    let point = self.pop()? as usize;
                    self.check_point(self.graphics_state.zp0, point)?;

                    let mut cvt_dist = if cvt_idx < self.cvt.len() {
                        self.cvt[cvt_idx]
                    } else {
                        F26Dot6::ZERO
                    };
                    let cur_proj = self.project(self.graphics_state.zp0, point);

                    if do_round {
                        let diff = (cvt_dist - cur_proj).abs();
                        if diff > self.graphics_state.control_value_cut_in {
                            cvt_dist = cur_proj;
                        }
                        cvt_dist = self.round_value(cvt_dist);
                    }

                    let delta = cvt_dist - cur_proj;
                    self.move_point(self.graphics_state.zp0, point, delta);
                    self.touch_point(self.graphics_state.zp0, point);
                    self.graphics_state.rp0 = point;
                    self.graphics_state.rp1 = point;
                }

                // -- MDRP (32 variants) --
                0xC0..=0xDF => {
                    self.exec_mdrp(opcode)?;
                }

                // -- MIRP (32 variants) --
                0xE0..=0xFF => {
                    self.exec_mirp(opcode)?;
                }

                0x32 | 0x33 => {
                    // SHP[a]: shift point by rp delta. a=0 uses rp2/zp1, a=1 uses rp1/zp0
                    let (rp, rp_zone) = if opcode & 1 != 0 {
                        (self.graphics_state.rp1, self.graphics_state.zp0)
                    } else {
                        (self.graphics_state.rp2, self.graphics_state.zp1)
                    };
                    self.check_point(rp_zone, rp)?;
                    let rp_cur = self.project(rp_zone, rp);
                    let rp_orig = self.dual_project(rp_zone, rp);
                    let delta = rp_cur - rp_orig;

                    let loop_val = self.graphics_state.loop_value;
                    self.graphics_state.loop_value = 1;

                    for _ in 0..loop_val {
                        let point = self.pop()? as usize;
                        self.check_point(self.graphics_state.zp2, point)?;
                        let cur = self.project(self.graphics_state.zp2, point);
                        self.move_point(
                            self.graphics_state.zp2,
                            point,
                            delta - (cur - self.dual_project(self.graphics_state.zp2, point)),
                        );
                        // Simpler: move by the same delta the reference point moved
                        self.touch_point(self.graphics_state.zp2, point);
                    }
                }

                0x38 => {
                    // SHPIX: shift point by pixel amount
                    let distance = F26Dot6(self.pop()?);
                    let loop_val = self.graphics_state.loop_value;
                    self.graphics_state.loop_value = 1;

                    for _ in 0..loop_val {
                        let point = self.pop()? as usize;
                        self.check_point(self.graphics_state.zp2, point)?;
                        self.move_point(self.graphics_state.zp2, point, distance);
                        self.touch_point(self.graphics_state.zp2, point);
                    }
                }

                0x3A | 0x3B => {
                    // MSIRP[a]: move stack indirect relative point. a=bit0 -> set rp0
                    let set_rp0 = opcode & 1 != 0;
                    let distance = F26Dot6(self.pop()?);
                    let point = self.pop()? as usize;
                    self.check_point(self.graphics_state.zp1, point)?;
                    self.check_point(self.graphics_state.zp0, self.graphics_state.rp0)?;

                    let cur_dist = self.project(self.graphics_state.zp1, point)
                        - self.project(self.graphics_state.zp0, self.graphics_state.rp0);
                    self.move_point(self.graphics_state.zp1, point, distance - cur_dist);
                    self.touch_point(self.graphics_state.zp1, point);

                    self.graphics_state.rp1 = self.graphics_state.rp0;
                    self.graphics_state.rp2 = point;
                    if set_rp0 {
                        self.graphics_state.rp0 = point;
                    }
                }

                0x3C => {
                    // ALIGNRP: align point with rp0
                    let loop_val = self.graphics_state.loop_value;
                    self.graphics_state.loop_value = 1;
                    self.check_point(self.graphics_state.zp0, self.graphics_state.rp0)?;

                    for _ in 0..loop_val {
                        let point = self.pop()? as usize;
                        self.check_point(self.graphics_state.zp1, point)?;
                        let dist = self.project(self.graphics_state.zp1, point)
                            - self.project(self.graphics_state.zp0, self.graphics_state.rp0);
                        self.move_point(self.graphics_state.zp1, point, -dist);
                        self.touch_point(self.graphics_state.zp1, point);
                    }
                }

                0x39 => {
                    // IP: interpolate point
                    let loop_val = self.graphics_state.loop_value;
                    self.graphics_state.loop_value = 1;

                    self.check_point(self.graphics_state.zp0, self.graphics_state.rp1)?;
                    self.check_point(self.graphics_state.zp1, self.graphics_state.rp2)?;

                    let rp1_orig =
                        self.dual_project(self.graphics_state.zp0, self.graphics_state.rp1);
                    let rp2_orig =
                        self.dual_project(self.graphics_state.zp1, self.graphics_state.rp2);
                    let rp1_cur = self.project(self.graphics_state.zp0, self.graphics_state.rp1);
                    let rp2_cur = self.project(self.graphics_state.zp1, self.graphics_state.rp2);

                    let org_range = rp2_orig.0 - rp1_orig.0;
                    let cur_range = rp2_cur.0 - rp1_cur.0;

                    for _ in 0..loop_val {
                        let point = self.pop()? as usize;
                        self.check_point(self.graphics_state.zp2, point)?;

                        let pt_orig = self.dual_project(self.graphics_state.zp2, point);
                        let pt_cur = self.project(self.graphics_state.zp2, point);

                        let new_pos = if org_range != 0 {
                            let factor = pt_orig.0 - rp1_orig.0;
                            rp1_cur.0
                                + ((factor as i64 * cur_range as i64) / org_range as i64) as i32
                        } else {
                            pt_cur.0
                        };

                        self.move_point(
                            self.graphics_state.zp2,
                            point,
                            F26Dot6(new_pos - pt_cur.0),
                        );
                        self.touch_point(self.graphics_state.zp2, point);
                    }
                }

                // -- GC, SCFS, MD --
                0x46 | 0x47 => {
                    // GC[a]: get coordinate. a=0 current, a=1 original
                    let point = self.pop()? as usize;
                    self.check_point(self.graphics_state.zp2, point)?;
                    let val = if opcode & 1 != 0 {
                        self.dual_project(self.graphics_state.zp2, point)
                    } else {
                        self.project(self.graphics_state.zp2, point)
                    };
                    self.push(val.0)?;
                }
                0x48 => {
                    // SCFS: set coordinate from stack
                    let val = self.pop()?;
                    let point = self.pop()? as usize;
                    self.check_point(self.graphics_state.zp2, point)?;

                    let cur = self.project(self.graphics_state.zp2, point);
                    self.move_point(self.graphics_state.zp2, point, F26Dot6(val) - cur);
                    self.touch_point(self.graphics_state.zp2, point);
                }
                0x49 | 0x4A => {
                    // MD[a]: measure distance. a=0 current, a=1 original
                    let p2 = self.pop()? as usize;
                    let p1 = self.pop()? as usize;
                    self.check_point(self.graphics_state.zp0, p1)?;
                    self.check_point(self.graphics_state.zp1, p2)?;

                    let dist = if opcode & 1 != 0 {
                        self.dual_project(self.graphics_state.zp1, p2)
                            - self.dual_project(self.graphics_state.zp0, p1)
                    } else {
                        self.project(self.graphics_state.zp1, p2)
                            - self.project(self.graphics_state.zp0, p1)
                    };
                    self.push(dist.0)?;
                }

                // -- MPPEM, MPS --
                0x4B => {
                    // MPPEM: measure pixels per em
                    self.push(self.ppem as i32)?;
                }
                0x4C => {
                    // MPS: measure point size (we use ppem as proxy)
                    self.push(self.ppem as i32)?;
                }

                0x30 | 0x31 => {
                    // IUP[a]: interpolate untouched points. 0x31=x, 0x30=y
                    let do_x = opcode == 0x31;
                    self.exec_iup(do_x);
                }

                0x80 => {
                    // FLIPPT: flip on-curve flag for points
                    let loop_val = self.graphics_state.loop_value;
                    self.graphics_state.loop_value = 1;
                    for _ in 0..loop_val {
                        let point = self.pop()? as usize;
                        let zone = self.get_zone_mut(self.graphics_state.zp0);
                        if point < zone.len() {
                            zone.on_curve[point] = !zone.on_curve[point];
                        }
                    }
                }
                0x81 => {
                    // FLIPRGON: flip range on
                    let hi = self.pop()? as usize;
                    let lo = self.pop()? as usize;
                    let zone = self.get_zone_mut(self.graphics_state.zp0);
                    for i in lo..=hi.min(zone.len().saturating_sub(1)) {
                        zone.on_curve[i] = true;
                    }
                }
                0x82 => {
                    // FLIPRGOFF: flip range off
                    let hi = self.pop()? as usize;
                    let lo = self.pop()? as usize;
                    let zone = self.get_zone_mut(self.graphics_state.zp0);
                    for i in lo..=hi.min(zone.len().saturating_sub(1)) {
                        zone.on_curve[i] = false;
                    }
                }

                // -- Delta instructions --
                0x5D => {
                    self.exec_deltap(0)?;
                } // DELTAP1
                0x71 => {
                    self.exec_deltap(16)?;
                } // DELTAP2
                0x72 => {
                    self.exec_deltap(32)?;
                } // DELTAP3
                0x73 => {
                    self.exec_deltac(0)?;
                } // DELTAC1
                0x74 => {
                    self.exec_deltac(16)?;
                } // DELTAC2
                0x75 => {
                    self.exec_deltac(32)?;
                } // DELTAC3

                // -- Scan/instruction control --
                0x85 => {
                    // SCANCTRL
                    let val = self.pop()?;
                    self.graphics_state.scan_control = val != 0;
                }
                0x8D => {
                    // SCANTYPE
                    let val = self.pop()?;
                    self.graphics_state.scan_type = val as u16;
                }
                0x8E => {
                    // INSTCTRL
                    let selector = self.pop()?;
                    let value = self.pop()?;
                    if (1..=2).contains(&selector) {
                        let bit = 1u8 << (selector - 1);
                        if value != 0 {
                            self.graphics_state.instruct_control |= bit;
                        } else {
                            self.graphics_state.instruct_control &= !bit;
                        }
                    }
                }

                0x88 => {
                    // GETINFO: return engine info.
                    //
                    // Mirror FreeType's v40 (modern ClearType) interpreter --
                    // the same defaults MuPDF gets when it calls FT with
                    // FT_LOAD_TARGET_LIGHT/NORMAL. Modern fonts branch on the
                    // subpixel/symmetrical-smoothing bits to disable X-axis
                    // hinting; claiming only "grayscale" (bit 12) as we did
                    // before makes fonts hint like they're rendering for
                    // 1998-era b/w targets, which diverges from MuPDF.
                    let selector = self.pop()?;
                    let mut result = 0i32;
                    // Bit 0: version. Return 40 (ClearType / v40 interpreter).
                    if selector & 1 != 0 {
                        result |= 40;
                    }
                    // Bit 1: rotated. We never rotate.
                    // Bit 2: stretched. We never stretch.
                    // Bit 5 -> bit 12: grayscale.
                    if selector & 32 != 0 {
                        result |= 1 << 12;
                    }
                    // Bit 6 -> bit 13: HINTING FOR SUBPIXEL. v40 has this on
                    // by default. This is the flag fonts check to skip X-axis
                    // CVT/MDAP calls that would otherwise snap stem positions.
                    if selector & 64 != 0 {
                        result |= 1 << 13;
                    }
                    // Bit 8 -> bit 15: VERTICAL LCD SUBPIXELS. Off (we render
                    // horizontally).
                    // Bit 10 -> bit 17: SUBPIXEL POSITIONED. On -- fonts that
                    // check this get hinted in a way that plays nicely with
                    // sub-pixel pen positioning (our 12-bin cache).
                    if selector & 1024 != 0 {
                        result |= 1 << 17;
                    }
                    // Bit 11 -> bit 18: SYMMETRICAL SMOOTHING. FT's default.
                    if selector & 2048 != 0 {
                        result |= 1 << 18;
                    }
                    // Bit 12 -> bit 19: CLEARTYPE HINTING AND GRAYSCALE.
                    if selector & 4096 != 0 {
                        result |= 1 << 19;
                    }
                    self.push(result)?;
                }

                // -- Miscellaneous no-ops and rarely-used opcodes --
                0x0E => {
                    // SFVTPV: set freedom vector to projection vector
                    self.graphics_state.freedom_vector = self.graphics_state.projection_vector;
                }
                0x0F => {
                    // ISECT: move point to intersection of two lines
                    // (p, a0, a1, b0, b1) -- quite rare, best-effort
                    let b1 = self.pop()? as usize;
                    let b0 = self.pop()? as usize;
                    let a1 = self.pop()? as usize;
                    let a0 = self.pop()? as usize;
                    let point = self.pop()? as usize;
                    self.exec_isect(point, a0, a1, b0, b1)?;
                }

                0x27 => {
                    // ALIGNPTS: align two points
                    let p2 = self.pop()? as usize;
                    let p1 = self.pop()? as usize;
                    self.check_point(self.graphics_state.zp1, p1)?;
                    self.check_point(self.graphics_state.zp0, p2)?;

                    let d1 = self.project(self.graphics_state.zp1, p1);
                    let d2 = self.project(self.graphics_state.zp0, p2);
                    let mid = F26Dot6((d1.0 + d2.0) / 2);
                    self.move_point(self.graphics_state.zp1, p1, mid - d1);
                    self.move_point(self.graphics_state.zp0, p2, mid - d2);
                }

                0x28 => {
                    // UTP: untouch point
                    let point = self.pop()? as usize;
                    let (fx, fy) = self.graphics_state.freedom_vector;
                    let zone = self.get_zone_mut(self.graphics_state.zp0);
                    if point < zone.len() {
                        if fx.abs() >= fy.abs() {
                            zone.touched_x[point] = false;
                        }
                        if fy.abs() >= fx.abs() {
                            zone.touched_y[point] = false;
                        }
                    }
                }

                0x29 => {
                    // Unknown/debug: treat as no-op
                }

                0x4D => {
                    // FLIPON: set auto_flip = true
                    self.graphics_state.auto_flip = true;
                }
                0x4E => {
                    // FLIPOFF: set auto_flip = false
                    self.graphics_state.auto_flip = false;
                }

                0x4F => {
                    // DEBUG: no-op in production
                    self.pop()?;
                }

                0x89 => {
                    // IDEF: define a user-defined instruction. We don't
                    // support user-defined opcodes, but we still need to
                    // skip past the body so prep can continue. Pop the
                    // opcode number, then scan forward to the matching
                    // ENDF (balancing nested FDEF/ENDF).
                    self.pop()?;
                    ip = self.skip_to_endf(instructions, ip)?;
                }

                0x34 | 0x35 => {
                    // SHC[a]: shift contour -- rarely used, degenerate case
                    let contour = self.pop()? as usize;
                    let (rp, rp_zone) = if opcode & 1 != 0 {
                        (self.graphics_state.rp1, self.graphics_state.zp0)
                    } else {
                        (self.graphics_state.rp2, self.graphics_state.zp1)
                    };
                    self.check_point(rp_zone, rp)?;
                    let rp_cur = self.project(rp_zone, rp);
                    let rp_orig = self.dual_project(rp_zone, rp);
                    let delta = rp_cur - rp_orig;

                    let zone = self.get_zone(self.graphics_state.zp2);
                    if contour < zone.contour_ends.len() {
                        let end = zone.contour_ends[contour];
                        let start = if contour > 0 {
                            zone.contour_ends[contour - 1] + 1
                        } else {
                            0
                        };
                        for i in start..=end.min(zone.len().saturating_sub(1)) {
                            let cur = self.project(self.graphics_state.zp2, i);
                            let orig = self.dual_project(self.graphics_state.zp2, i);
                            self.move_point(self.graphics_state.zp2, i, delta - (cur - orig));
                        }
                    }
                }

                0x36 | 0x37 => {
                    // SHZ[a]: shift zone
                    let zone_idx = self.pop()? as usize;
                    if zone_idx > 1 {
                        return Err(HintError::InvalidReference);
                    }
                    let (rp, rp_zone) = if opcode & 1 != 0 {
                        (self.graphics_state.rp1, self.graphics_state.zp0)
                    } else {
                        (self.graphics_state.rp2, self.graphics_state.zp1)
                    };
                    self.check_point(rp_zone, rp)?;
                    let rp_cur = self.project(rp_zone, rp);
                    let rp_orig = self.dual_project(rp_zone, rp);
                    let delta = rp_cur - rp_orig;

                    let n = self.get_zone(zone_idx).len();
                    for i in 0..n {
                        let cur = self.project(zone_idx, i);
                        let orig = self.dual_project(zone_idx, i);
                        self.move_point(zone_idx, i, delta - (cur - orig));
                    }
                }

                // Catch-all for undefined opcodes
                op => {
                    return Err(HintError::InvalidInstruction(op));
                }
            }
        }

        Ok(())
    }

    // -- Complex instruction helpers --

    fn exec_mdrp(&mut self, opcode: u8) -> Result<(), HintError> {
        let flags = opcode - 0xC0;
        let set_rp0 = flags & 0x10 != 0;
        let keep_min = flags & 0x08 != 0;
        let do_round = flags & 0x04 != 0;

        let point = self.pop()? as usize;
        self.check_point(self.graphics_state.zp1, point)?;
        self.check_point(self.graphics_state.zp0, self.graphics_state.rp0)?;

        // Measure original distance via dual projection
        let org_dist = self.dual_project(self.graphics_state.zp1, point)
            - self.dual_project(self.graphics_state.zp0, self.graphics_state.rp0);

        // Apply single-width test
        let mut distance = if self.graphics_state.single_width_cut_in.0 > 0
            && (org_dist - self.graphics_state.single_width_value).abs().0
                < self.graphics_state.single_width_cut_in.0
        {
            if org_dist.0 >= 0 {
                self.graphics_state.single_width_value
            } else {
                -self.graphics_state.single_width_value
            }
        } else {
            org_dist
        };

        if do_round {
            distance = self.round_value(distance);
        }

        if keep_min {
            let min = self.graphics_state.minimum_distance;
            if org_dist.0 >= 0 {
                if distance.0 < min.0 {
                    distance = min;
                }
            } else if distance.0 > -min.0 {
                distance = F26Dot6(-min.0);
            }
        }

        // Current distance between the two points
        let cur_dist = self.project(self.graphics_state.zp1, point)
            - self.project(self.graphics_state.zp0, self.graphics_state.rp0);

        self.move_point(self.graphics_state.zp1, point, distance - cur_dist);
        self.touch_point(self.graphics_state.zp1, point);

        self.graphics_state.rp1 = self.graphics_state.rp0;
        self.graphics_state.rp2 = point;
        if set_rp0 {
            self.graphics_state.rp0 = point;
        }

        Ok(())
    }

    fn exec_mirp(&mut self, opcode: u8) -> Result<(), HintError> {
        let flags = opcode - 0xE0;
        let set_rp0 = flags & 0x10 != 0;
        let keep_min = flags & 0x08 != 0;
        let do_round = flags & 0x04 != 0;

        let cvt_idx = self.pop()? as usize;
        let point = self.pop()? as usize;
        self.check_point(self.graphics_state.zp1, point)?;
        self.check_point(self.graphics_state.zp0, self.graphics_state.rp0)?;

        let mut cvt_dist = if cvt_idx < self.cvt.len() {
            self.cvt[cvt_idx]
        } else {
            F26Dot6::ZERO
        };

        // Measure original distance via dual projection
        let org_dist = self.dual_project(self.graphics_state.zp1, point)
            - self.dual_project(self.graphics_state.zp0, self.graphics_state.rp0);

        // Auto-flip: if the CVT distance disagrees in sign with the original, negate CVT
        if self.graphics_state.auto_flip && (cvt_dist.0 ^ org_dist.0) < 0 {
            cvt_dist = -cvt_dist;
        }

        // Single-width test
        if self.graphics_state.single_width_cut_in.0 > 0
            && (cvt_dist - self.graphics_state.single_width_value).abs().0
                < self.graphics_state.single_width_cut_in.0
        {
            cvt_dist = if org_dist.0 >= 0 {
                self.graphics_state.single_width_value
            } else {
                -self.graphics_state.single_width_value
            };
        }

        // Control value cut-in: use original distance if CVT is too different
        let mut distance =
            if (org_dist - cvt_dist).abs().0 > self.graphics_state.control_value_cut_in.0 {
                org_dist
            } else {
                cvt_dist
            };

        if do_round {
            distance = self.round_value(distance);
        }

        if keep_min {
            let min = self.graphics_state.minimum_distance;
            if org_dist.0 >= 0 {
                if distance.0 < min.0 {
                    distance = min;
                }
            } else if distance.0 > -min.0 {
                distance = F26Dot6(-min.0);
            }
        }

        let cur_dist = self.project(self.graphics_state.zp1, point)
            - self.project(self.graphics_state.zp0, self.graphics_state.rp0);

        self.move_point(self.graphics_state.zp1, point, distance - cur_dist);
        self.touch_point(self.graphics_state.zp1, point);

        self.graphics_state.rp1 = self.graphics_state.rp0;
        self.graphics_state.rp2 = point;
        if set_rp0 {
            self.graphics_state.rp0 = point;
        }

        Ok(())
    }

    /// IUP: interpolate untouched points.
    fn exec_iup(&mut self, do_x: bool) {
        let n = self.glyph.len();
        if n == 0 {
            return;
        }

        // Copy contour_ends so we don't hold a borrow on self.glyph
        let contour_ends: Vec<usize> = self.glyph.contour_ends.clone();

        let mut contour_start = 0usize;
        for &contour_end in &contour_ends {
            if contour_end >= n {
                break;
            }
            let cstart = contour_start;
            let cend = contour_end;
            if cend < cstart {
                contour_start = cend + 1;
                continue;
            }

            // Collect touched point indices for this contour
            let mut touched_indices: Vec<usize> = Vec::new();
            for i in cstart..=cend {
                let is_touched = if do_x {
                    self.glyph.touched_x[i]
                } else {
                    self.glyph.touched_y[i]
                };
                if is_touched {
                    touched_indices.push(i);
                }
            }

            if touched_indices.is_empty() {
                contour_start = cend + 1;
                continue;
            }

            if touched_indices.len() == 1 {
                let ti = touched_indices[0];
                let delta = if do_x {
                    self.glyph.current[ti].x.0 - self.glyph.original[ti].x.0
                } else {
                    self.glyph.current[ti].y.0 - self.glyph.original[ti].y.0
                };
                for i in cstart..=cend {
                    if i == ti {
                        continue;
                    }
                    if do_x {
                        self.glyph.current[i].x.0 = self.glyph.original[i].x.0 + delta;
                    } else {
                        self.glyph.current[i].y.0 = self.glyph.original[i].y.0 + delta;
                    }
                }
                contour_start = cend + 1;
                continue;
            }

            // Multiple touched points: interpolate between consecutive pairs (wrapping)
            let tcount = touched_indices.len();
            for t in 0..tcount {
                let t1_idx = touched_indices[t];
                let t2_idx = touched_indices[(t + 1) % tcount];

                let mut i = t1_idx;
                loop {
                    i += 1;
                    if i > cend {
                        i = cstart;
                    }
                    if i == t2_idx {
                        break;
                    }

                    let is_touched = if do_x {
                        self.glyph.touched_x[i]
                    } else {
                        self.glyph.touched_y[i]
                    };
                    if !is_touched {
                        self.interpolate_point(i, t1_idx, t2_idx, do_x);
                    }
                }
            }

            contour_start = cend + 1;
        }
    }

    /// Interpolate a single untouched point between two touched reference points.
    fn interpolate_point(&mut self, point: usize, ref1: usize, ref2: usize, do_x: bool) {
        let (orig_val, ref1_orig, ref2_orig, ref1_cur, ref2_cur) = if do_x {
            (
                self.glyph.original[point].x.0,
                self.glyph.original[ref1].x.0,
                self.glyph.original[ref2].x.0,
                self.glyph.current[ref1].x.0,
                self.glyph.current[ref2].x.0,
            )
        } else {
            (
                self.glyph.original[point].y.0,
                self.glyph.original[ref1].y.0,
                self.glyph.original[ref2].y.0,
                self.glyph.current[ref1].y.0,
                self.glyph.current[ref2].y.0,
            )
        };

        // Sort reference points by original coordinate
        let (o_lo, o_hi, c_lo, c_hi) = if ref1_orig <= ref2_orig {
            (ref1_orig, ref2_orig, ref1_cur, ref2_cur)
        } else {
            (ref2_orig, ref1_orig, ref2_cur, ref1_cur)
        };

        let new_val = if orig_val <= o_lo {
            // Below the lower reference: shift by lower ref's delta
            c_lo + (orig_val - o_lo)
        } else if orig_val >= o_hi {
            // Above the upper reference: shift by upper ref's delta
            c_hi + (orig_val - o_hi)
        } else {
            // Between the two: linear interpolation
            let org_range = o_hi - o_lo;
            if org_range == 0 {
                c_lo
            } else {
                let factor = orig_val - o_lo;
                c_lo + ((factor as i64 * (c_hi - c_lo) as i64) / org_range as i64) as i32
            }
        };

        if do_x {
            self.glyph.current[point].x.0 = new_val;
        } else {
            self.glyph.current[point].y.0 = new_val;
        }
    }

    /// DELTAP: pixel adjustment at specific ppem values.
    fn exec_deltap(&mut self, ppem_offset: u16) -> Result<(), HintError> {
        let n = self.pop()? as u32;
        for _ in 0..n {
            let arg = self.pop()?;
            let point = self.pop()? as usize;

            let ppem_target =
                ((arg >> 4) & 0xF) as u16 + self.graphics_state.delta_base + ppem_offset;
            if ppem_target == self.ppem {
                let magnitude = arg & 0xF;
                let steps = if magnitude >= 8 {
                    -(16 - magnitude)
                } else {
                    magnitude + 1
                };
                let distance = F26Dot6(steps * (1 << (6 - self.graphics_state.delta_shift as i32)));

                self.check_point(self.graphics_state.zp0, point)?;
                self.move_point(self.graphics_state.zp0, point, distance);
                self.touch_point(self.graphics_state.zp0, point);
            }
        }
        Ok(())
    }

    /// DELTAC: CVT adjustment at specific ppem values.
    fn exec_deltac(&mut self, ppem_offset: u16) -> Result<(), HintError> {
        let n = self.pop()? as u32;
        for _ in 0..n {
            let arg = self.pop()?;
            let cvt_idx = self.pop()? as usize;

            let ppem_target =
                ((arg >> 4) & 0xF) as u16 + self.graphics_state.delta_base + ppem_offset;
            if ppem_target == self.ppem {
                let magnitude = arg & 0xF;
                let steps = if magnitude >= 8 {
                    -(16 - magnitude)
                } else {
                    magnitude + 1
                };
                let delta = steps * (1 << (6 - self.graphics_state.delta_shift as i32));
                if cvt_idx < self.cvt.len() {
                    self.cvt[cvt_idx].0 += delta;
                }
            }
        }
        Ok(())
    }

    /// Compute a unit vector from point p1 to point p2 (or perpendicular).
    fn compute_line_vector(
        &self,
        z1: usize,
        p1: usize,
        z2: usize,
        p2: usize,
        perpendicular: bool,
    ) -> (i32, i32) {
        let zone1 = self.get_zone(z1);
        let zone2 = self.get_zone(z2);

        if p1 >= zone1.len() || p2 >= zone2.len() {
            return (F2DOT14_ONE, 0); // fallback to x-axis
        }

        let dx = zone2.current[p2].x.0 as i64 - zone1.current[p1].x.0 as i64;
        let dy = zone2.current[p2].y.0 as i64 - zone1.current[p1].y.0 as i64;

        Self::normalize_line_vector(dx, dy, perpendicular)
    }

    /// Compute a unit vector from point p1 to point p2 using ORIGINAL (pre-
    /// hinted) positions. Used by SDPVTL to set the dual projection vector.
    fn compute_line_vector_original(
        &self,
        z1: usize,
        p1: usize,
        z2: usize,
        p2: usize,
        perpendicular: bool,
    ) -> (i32, i32) {
        let zone1 = self.get_zone(z1);
        let zone2 = self.get_zone(z2);

        if p1 >= zone1.len() || p2 >= zone2.len() {
            return (F2DOT14_ONE, 0);
        }

        let dx = zone2.original[p2].x.0 as i64 - zone1.original[p1].x.0 as i64;
        let dy = zone2.original[p2].y.0 as i64 - zone1.original[p1].y.0 as i64;

        Self::normalize_line_vector(dx, dy, perpendicular)
    }

    fn normalize_line_vector(dx: i64, dy: i64, perpendicular: bool) -> (i32, i32) {
        if dx == 0 && dy == 0 {
            // Degenerate: FT falls back to X axis.
            return (F2DOT14_ONE, 0);
        }
        let (vx, vy) = if perpendicular { (-dy, dx) } else { (dx, dy) };

        let len = ((vx * vx + vy * vy) as f64).sqrt();
        let nx = ((vx as f64 / len) * F2DOT14_ONE as f64) as i32;
        let ny = ((vy as f64 / len) * F2DOT14_ONE as f64) as i32;
        (nx, ny)
    }

    /// ISECT: compute intersection of two lines and move point there.
    fn exec_isect(
        &mut self,
        point: usize,
        a0: usize,
        a1: usize,
        b0: usize,
        b1: usize,
    ) -> Result<(), HintError> {
        self.check_point(self.graphics_state.zp2, point)?;
        self.check_point(self.graphics_state.zp1, a0)?;
        self.check_point(self.graphics_state.zp1, a1)?;
        self.check_point(self.graphics_state.zp0, b0)?;
        self.check_point(self.graphics_state.zp0, b1)?;

        let za = self.get_zone(self.graphics_state.zp1);
        let ax0 = za.current[a0].x.0 as i64;
        let ay0 = za.current[a0].y.0 as i64;
        let ax1 = za.current[a1].x.0 as i64;
        let ay1 = za.current[a1].y.0 as i64;

        let zb = self.get_zone(self.graphics_state.zp0);
        let bx0 = zb.current[b0].x.0 as i64;
        let by0 = zb.current[b0].y.0 as i64;
        let bx1 = zb.current[b1].x.0 as i64;
        let by1 = zb.current[b1].y.0 as i64;

        let dax = ax1 - ax0;
        let day = ay1 - ay0;
        let dbx = bx1 - bx0;
        let dby = by1 - by0;

        let denom = dax * dby - day * dbx;
        if denom == 0 {
            // Parallel lines: place at midpoint of a0 and b0
            let zone = self.get_zone_mut(self.graphics_state.zp2);
            zone.current[point].x.0 = ((ax0 + bx0) / 2) as i32;
            zone.current[point].y.0 = ((ay0 + by0) / 2) as i32;
        } else {
            let t_num = (bx0 - ax0) * dby - (by0 - ay0) * dbx;
            let ix = ax0 + (t_num * dax) / denom;
            let iy = ay0 + (t_num * day) / denom;
            let zone = self.get_zone_mut(self.graphics_state.zp2);
            zone.current[point].x.0 = ix as i32;
            zone.current[point].y.0 = iy as i32;
        }

        self.touch_point(self.graphics_state.zp2, point);
        Ok(())
    }

    // -- IF/ELSE/EIF skip helpers --

    /// Skip forward from inside a false IF to the matching ELSE or EIF.
    /// Returns the new IP (after the ELSE or EIF opcode).
    fn skip_to_else_or_eif(&self, code: &[u8], mut ip: usize) -> Result<usize, HintError> {
        let len = code.len();
        let mut depth = 1u32;
        while ip < len {
            match code[ip] {
                0x58 => {
                    depth += 1;
                    ip += 1;
                } // nested IF
                0x59 => {
                    // EIF
                    depth -= 1;
                    ip += 1;
                    if depth == 0 {
                        return Ok(ip);
                    }
                }
                0x1B if depth == 1 => {
                    // ELSE at our level
                    ip += 1;
                    return Ok(ip);
                }
                // Skip inline push data
                0x40 => {
                    ip += 1;
                    if ip < len {
                        ip += code[ip] as usize + 1;
                    } else {
                        ip += 1;
                    }
                }
                0x41 => {
                    ip += 1;
                    if ip < len {
                        ip += code[ip] as usize * 2 + 1;
                    } else {
                        ip += 1;
                    }
                }
                0xB0..=0xB7 => {
                    ip += (code[ip] - 0xB0 + 1) as usize + 1;
                }
                0xB8..=0xBF => {
                    ip += (code[ip] - 0xB8 + 1) as usize * 2 + 1;
                }
                _ => {
                    ip += 1;
                }
            }
        }
        Err(HintError::InvalidInstruction(0x58))
    }

    /// Skip forward from an ELSE (when we executed the IF-true branch) to matching EIF.
    fn skip_to_eif(&self, code: &[u8], mut ip: usize) -> Result<usize, HintError> {
        let len = code.len();
        let mut depth = 1u32;
        while ip < len {
            match code[ip] {
                0x58 => {
                    depth += 1;
                    ip += 1;
                }
                0x59 => {
                    depth -= 1;
                    ip += 1;
                    if depth == 0 {
                        return Ok(ip);
                    }
                }
                0x40 => {
                    ip += 1;
                    if ip < len {
                        ip += code[ip] as usize + 1;
                    } else {
                        ip += 1;
                    }
                }
                0x41 => {
                    ip += 1;
                    if ip < len {
                        ip += code[ip] as usize * 2 + 1;
                    } else {
                        ip += 1;
                    }
                }
                0xB0..=0xB7 => {
                    ip += (code[ip] - 0xB0 + 1) as usize + 1;
                }
                0xB8..=0xBF => {
                    ip += (code[ip] - 0xB8 + 1) as usize * 2 + 1;
                }
                _ => {
                    ip += 1;
                }
            }
        }
        Err(HintError::InvalidInstruction(0x1B))
    }

    /// Skip forward to the matching ENDF, balancing nested FDEF/IDEF blocks.
    /// Returns the IP just past the ENDF. Used by IDEF (we don't support user
    /// instruction definitions, but we still need to step over the body).
    fn skip_to_endf(&self, code: &[u8], mut ip: usize) -> Result<usize, HintError> {
        let len = code.len();
        let mut depth = 1u32;
        while ip < len {
            match code[ip] {
                0x2C | 0x89 => {
                    // Nested FDEF or IDEF
                    depth += 1;
                    ip += 1;
                }
                0x2D => {
                    depth -= 1;
                    ip += 1;
                    if depth == 0 {
                        return Ok(ip);
                    }
                }
                // Skip inline push data
                0x40 => {
                    ip += 1;
                    if ip < len {
                        ip += code[ip] as usize + 1;
                    } else {
                        ip += 1;
                    }
                }
                0x41 => {
                    ip += 1;
                    if ip < len {
                        ip += code[ip] as usize * 2 + 1;
                    } else {
                        ip += 1;
                    }
                }
                0xB0..=0xB7 => {
                    ip += (code[ip] - 0xB0 + 1) as usize + 1;
                }
                0xB8..=0xBF => {
                    ip += (code[ip] - 0xB8 + 1) as usize * 2 + 1;
                }
                _ => {
                    ip += 1;
                }
            }
        }
        Err(HintError::InvalidInstruction(0x89))
    }

    /// Compute a relative jump. The offset is relative to the position BEFORE the jump
    /// instruction's operands were popped, which for JMPR is the position of the next
    /// instruction. TrueType spec: ip_new = ip_before_operand + offset.
    /// We receive `ip` as the position of the next instruction (already advanced past opcode).
    /// The offset is relative to the point just before the instruction. For JMPR the operand
    /// is on the stack, the spec says offset is from the JMPR opcode itself.
    /// However, by convention, offset is relative to the start of the jump instruction.
    /// ip here is already past the opcode byte, so the jump base is ip - 1.
    fn jump_relative(&self, ip: usize, offset: i32) -> Result<usize, HintError> {
        // The spec says the offset is relative to the byte position of the instruction.
        // ip is currently one past the opcode, so the instruction was at ip - 1.
        let base = ip as i64 - 1;
        let target = base + offset as i64;
        if target < 0 {
            return Err(HintError::InvalidInstruction(0x1C));
        }
        Ok(target as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vm() -> Vm {
        Vm::new(256, 32, 64, 16)
    }

    #[test]
    fn test_push_pop() {
        let mut vm = make_vm();
        // NPUSHB: push 3 bytes [10, 20, 30]
        let code = [0x40, 3, 10, 20, 30];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![10, 20, 30]);
    }

    #[test]
    fn test_pushw() {
        let mut vm = make_vm();
        // NPUSHW: push 1 signed word (0xFF00 = -256 as i16, sign-extended to i32)
        let code = [0x41, 1, 0xFF, 0x00];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![-256]);
    }

    #[test]
    fn test_arithmetic() {
        let mut vm = make_vm();
        // Push 64 (1.0 in 26.6) and 128 (2.0), then ADD
        let code = [0x40, 2, 64, 128, 0x60];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![192]); // 3.0 in 26.6
    }

    #[test]
    fn test_mul() {
        let mut vm = make_vm();
        // Push 128, 128 (2.0, 2.0 in 26.6), then MUL -> 4.0 = 256
        let code = [0x40, 2, 128, 128, 0x63];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![256]); // 128*128 >> 6 = 256
    }

    #[test]
    fn test_dup_swap() {
        let mut vm = make_vm();
        let code = [0x40, 2, 5, 10, 0x23]; // push 5, 10, SWAP
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![10, 5]);
    }

    #[test]
    fn test_if_true() {
        let mut vm = make_vm();
        // Push 1 (true), IF, push 42, EIF
        let code = [0x40, 1, 1, 0x58, 0x40, 1, 42, 0x59];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![42]);
    }

    #[test]
    fn test_if_false() {
        let mut vm = make_vm();
        // Push 0 (false), IF, push 42, EIF
        let code = [0x40, 1, 0, 0x58, 0x40, 1, 42, 0x59];
        vm.execute(&code).unwrap();
        assert!(vm.stack.is_empty()); // 42 was not pushed
    }

    #[test]
    fn test_if_else() {
        let mut vm = make_vm();
        // Push 0, IF, push 10, ELSE, push 20, EIF
        let code = [0x40, 1, 0, 0x58, 0x40, 1, 10, 0x1B, 0x40, 1, 20, 0x59];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![20]);
    }

    #[test]
    fn test_fdef_call() {
        let mut vm = make_vm();
        // Define function 0 that pushes 99, then call it
        // PUSH 0, FDEF, PUSH 99, ENDF, PUSH 0, CALL
        let code = [
            0x40, 1, 0,    // push 0 (function number)
            0x2C, // FDEF
            0x40, 1, 99,   // push 99
            0x2D, // ENDF
            0x40, 1, 0,    // push 0 (function number)
            0x2B, // CALL
        ];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![99]);
    }

    #[test]
    fn test_round_to_grid() {
        let vm = make_vm();
        // 0.6 pixels = 38 in 26.6, rounds to 1.0 = 64
        assert_eq!(vm.round_value(F26Dot6(38)), F26Dot6(64));
        // 0.4 pixels = 26 in 26.6, rounds to 0.0 = 0... but the spec says round
        // should return at least engine compensation, which we treat as 0 for value=0.
        // Actually 26 + 32 = 58, & !63 = 0
        assert_eq!(vm.round_value(F26Dot6(26)), F26Dot6(0));
        // Exact 1.0
        assert_eq!(vm.round_value(F26Dot6(64)), F26Dot6(64));
    }

    #[test]
    fn test_comparison() {
        let mut vm = make_vm();
        // Push 5, 10, LT -> 1 (true)
        let code = [0x40, 2, 5, 10, 0x50];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![1]);
    }

    #[test]
    fn test_storage() {
        let mut vm = make_vm();
        // Push location=0, value=42, WS. Then push 0, RS.
        let code = [0x40, 2, 0, 42, 0x42, 0x40, 1, 0, 0x43];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![42]);
    }

    #[test]
    fn test_cvt() {
        let mut vm = make_vm();
        vm.cvt = vec![F26Dot6::ZERO; 4];

        // Write CVT[0] = 128 (pixel units). Then read it back.
        // Push idx=0, val=128, WCVTP. Push 0, RCVT.
        let code = [0x40, 2, 0, 128, 0x44, 0x40, 1, 0, 0x45];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![128]);
    }

    #[test]
    fn test_mppem() {
        let mut vm = make_vm();
        vm.ppem = 12;
        let code = [0x4B]; // MPPEM
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![12]);
    }

    #[test]
    fn test_instruction_limit() {
        let mut vm = make_vm();
        // Tight jump loop: PUSH offset, JMPR (jumps back to the PUSH)
        // PUSH 1 byte offset=-2 (0xFE as signed), but we need to push -2 as i32.
        // Use NPUSHW to push -2.
        // At ip=0: NPUSHW, 1, 0xFF, 0xFE -> pushes -2.
        // At ip=4: JMPR -> pops -2, jumps to ip=4-1+(-2)=1. Hmm.
        // Let's use PUSHB + loop that eventually hits the limit.
        // Actually this is tricky with unsigned PUSHB. Let's just test the limit
        // by running a long no-op stream.
        vm.instruction_count = MAX_INSTRUCTIONS - 1;
        // Two no-ops would exceed
        let code = [0x4B, 0x4B]; // MPPEM, MPPEM
        let result = vm.execute(&code);
        assert!(result.is_err());
    }

    #[test]
    fn test_stack_overflow() {
        let mut vm = Vm::new(2, 4, 4, 4); // max stack depth = 2
        let code = [0x40, 3, 1, 2, 3]; // push 3 values
        let result = vm.execute(&code);
        assert!(matches!(result, Err(HintError::StackError)));
    }

    #[test]
    fn test_svtca() {
        let mut vm = make_vm();
        // SVTCA[1] = x-axis
        let code = [0x01];
        vm.execute(&code).unwrap();
        assert_eq!(vm.graphics_state.projection_vector, (F2DOT14_ONE, 0));
        assert_eq!(vm.graphics_state.freedom_vector, (F2DOT14_ONE, 0));

        // SVTCA[0] = y-axis
        let code = [0x00];
        vm.execute(&code).unwrap();
        assert_eq!(vm.graphics_state.projection_vector, (0, F2DOT14_ONE));
        assert_eq!(vm.graphics_state.freedom_vector, (0, F2DOT14_ONE));
    }

    #[test]
    fn test_roll() {
        let mut vm = make_vm();
        // Push 1, 2, 3, ROLL -> 2, 3, 1
        let code = [0x40, 3, 1, 2, 3, 0x8A];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![2, 3, 1]);
    }

    #[test]
    fn test_neg_abs() {
        let mut vm = make_vm();
        // NPUSHW to push -100, then ABS
        let code = [0x41, 1, 0xFF, 0x9C, 0x64]; // -100, ABS
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![100]);
    }

    #[test]
    fn test_floor_ceiling() {
        let mut vm = make_vm();
        // Push 100 (1.5625 pixels in 26.6), FLOOR -> 64 (1.0)
        let code = [0x40, 1, 100, 0x66];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![64]);

        vm.stack.clear();
        // Push 100, CEILING -> 128 (2.0)
        let code = [0x40, 1, 100, 0x67];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![128]);
    }

    #[test]
    fn test_not_and_or() {
        let mut vm = make_vm();
        let code = [0x40, 1, 0, 0x5C]; // NOT(0) -> 1
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![1]);

        vm.stack.clear();
        let code = [0x40, 2, 1, 0, 0x5A]; // AND(1, 0) -> 0
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![0]);

        vm.stack.clear();
        let code = [0x40, 2, 1, 0, 0x5B]; // OR(1, 0) -> 1
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![1]);
    }

    #[test]
    fn test_sdb_sds() {
        let mut vm = make_vm();
        // Push 20 then SDB, then push 4 then SDS.
        let code = [0x40, 1, 20, 0x5E, 0x40, 1, 4, 0x5F];
        vm.execute(&code).unwrap();
        assert_eq!(vm.graphics_state.delta_base, 20);
        assert_eq!(vm.graphics_state.delta_shift, 4);

        // Out-of-range SDS is silently ignored (FT raises in pedantic mode).
        let code = [0x40, 1, 9, 0x5F];
        vm.execute(&code).unwrap();
        assert_eq!(vm.graphics_state.delta_shift, 4);
    }

    #[test]
    fn test_round_opcodes() {
        let mut vm = make_vm();
        // Default round state is RoundToGrid.
        // Push 100 (~1.5625 px), ROUND[00] -> 128 (2.0)
        let code = [0x40, 1, 100, 0x68];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![128]);

        // ROUND[01]/[10]/[11] are the same with compensation=0.
        vm.stack.clear();
        let code = [0x40, 1, 100, 0x6B];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![128]);
    }

    #[test]
    fn test_nround_opcodes() {
        let mut vm = make_vm();
        // NROUND is a no-op with compensation=0.
        let code = [0x40, 1, 100, 0x6C];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![100]);

        vm.stack.clear();
        let code = [0x40, 1, 42, 0x6F];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![42]);
    }

    #[test]
    fn test_sangw_aa_noop() {
        let mut vm = make_vm();
        // SANGW pops 1 and ignores it.
        let code = [0x40, 1, 99, 0x7E];
        vm.execute(&code).unwrap();
        assert!(vm.stack.is_empty());

        // AA pops 1 and ignores it.
        let code = [0x40, 1, 7, 0x7F];
        vm.execute(&code).unwrap();
        assert!(vm.stack.is_empty());
    }

    #[test]
    fn test_getdata() {
        let mut vm = make_vm();
        let code = [0x92];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![17]);
    }

    #[test]
    fn test_reserved_opcodes_noop() {
        let mut vm = make_vm();
        // 0x83, 0x84, 0x8F, 0x90, 0x91 should not fail.
        let code = [0x83, 0x84, 0x8F, 0x90, 0x91];
        vm.execute(&code).unwrap();
        assert!(vm.stack.is_empty());
    }

    #[test]
    fn test_idef_skips_body() {
        let mut vm = make_vm();
        // IDEF 0x99 ... ENDF, then push 42.
        // Push 0x99 (opcode for new instruction), IDEF, PUSH 100, ENDF, PUSH 42.
        let code = [
            0x40, 1, 0x99, // push 0x99
            0x89, // IDEF
            0x40, 1, 100,  // push 100 (inside IDEF body, skipped)
            0x2D, // ENDF
            0x40, 1, 42, // push 42
        ];
        vm.execute(&code).unwrap();
        assert_eq!(vm.stack, vec![42]);
    }

    #[test]
    fn test_sdpvtl_horizontal() {
        // Set up glyph zone with two points on a horizontal line.
        let mut vm = make_vm();
        let pts = vec![
            (F26Dot6(0), F26Dot6(0), true),
            (F26Dot6(640), F26Dot6(0), true), // 10 px to the right
        ];
        vm.setup_glyph_zone(&pts, &[1], F26Dot6(640), F26Dot6(0));

        // SDPVTL[0] with p1=0, p2=1: projection vector -> x-axis.
        let code = [0x40, 2, 1, 0, 0x86];
        vm.execute(&code).unwrap();
        let (px, py) = vm.graphics_state.projection_vector;
        // Should be close to (0x4000, 0).
        assert!(px.abs() > 0x3F00 && py.abs() < 0x100);
    }

    #[test]
    fn test_sdpvtl_perpendicular() {
        let mut vm = make_vm();
        let pts = vec![
            (F26Dot6(0), F26Dot6(0), true),
            (F26Dot6(640), F26Dot6(0), true),
        ];
        vm.setup_glyph_zone(&pts, &[1], F26Dot6(640), F26Dot6(0));

        // SDPVTL[1] -> rotates 90deg CCW. Horizontal line becomes vertical vec.
        let code = [0x40, 2, 1, 0, 0x87];
        vm.execute(&code).unwrap();
        let (px, py) = vm.graphics_state.projection_vector;
        // Should be close to (0, 0x4000) or (0, -0x4000).
        assert!(px.abs() < 0x100 && py.abs() > 0x3F00);
    }
}
