//! Type3 font CharProc outline extraction.
//!
//! Parses a CharProc content stream (mini PDF drawing commands) and
//! converts the path data to a GlyphOutline for rendering. Also provides
//! compact binary serialization for transport via the AssetStore.

use super::ttf::{Contour, GlyphOutline, OutlinePoint, StemHints};

/// Extract a glyph outline from a Type3 CharProc content stream.
///
/// Interprets path construction operators (m, l, c, v, y, re, h) and
/// painting operators (f, S, B, etc.) from the raw byte stream. Applies
/// the font matrix to transform from glyph space to a 1000-UPM coordinate
/// system (matching Type1/CFF conventions).
///
/// Returns None if the stream produces no path data.
pub fn extract_charproc_outline(data: &[u8], font_matrix: [f64; 6]) -> Option<GlyphOutline> {
    let mut interp = CharProcInterp::new();
    interp.run(data);

    if interp.contours.is_empty() {
        return None;
    }

    // Apply font matrix to all points. Type3 FontMatrix is typically
    // [0.001 0 0 0.001 0 0] which scales 1000-unit glyph coords to
    // 1-unit text space. We want 1000-UPM output, so multiply by 1000.
    let scale = 1000.0;
    let (a, b, c, d, e, f) = (
        font_matrix[0] * scale,
        font_matrix[1] * scale,
        font_matrix[2] * scale,
        font_matrix[3] * scale,
        font_matrix[4] * scale,
        font_matrix[5] * scale,
    );

    let mut x_min = f64::MAX;
    let mut y_min = f64::MAX;
    let mut x_max = f64::MIN;
    let mut y_max = f64::MIN;

    let contours: Vec<Contour> = interp
        .contours
        .iter()
        .filter(|pts| pts.len() >= 2)
        .map(|pts| {
            let points: Vec<OutlinePoint> = pts
                .iter()
                .map(|p| {
                    let tx = a * p.0 + c * p.1 + e;
                    let ty = b * p.0 + d * p.1 + f;
                    x_min = x_min.min(tx);
                    y_min = y_min.min(ty);
                    x_max = x_max.max(tx);
                    y_max = y_max.max(ty);
                    OutlinePoint {
                        x: tx,
                        y: ty,
                        on_curve: p.2,
                    }
                })
                .collect();
            Contour { points }
        })
        .collect();

    if contours.is_empty() {
        return None;
    }

    let bounds = (
        x_min as i16,
        y_min as i16,
        x_max.ceil() as i16,
        y_max.ceil() as i16,
    );

    Some(GlyphOutline {
        contours,
        bounds,
        stem_hints: StemHints::default(),
    })
}

/// Serialize a GlyphOutline to compact binary format.
///
/// Format: contour_count(u32), then per contour: point_count(u32),
/// then per point: x(f64 LE), y(f64 LE), on_curve(u8).
/// Ends with bounds: x_min(i16 LE), y_min(i16 LE), x_max(i16 LE), y_max(i16 LE).
pub fn serialize_outline(outline: &GlyphOutline) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(outline.contours.len() as u32).to_le_bytes());
    for contour in &outline.contours {
        buf.extend_from_slice(&(contour.points.len() as u32).to_le_bytes());
        for p in &contour.points {
            buf.extend_from_slice(&p.x.to_le_bytes());
            buf.extend_from_slice(&p.y.to_le_bytes());
            buf.push(if p.on_curve { 1 } else { 0 });
        }
    }
    buf.extend_from_slice(&outline.bounds.0.to_le_bytes());
    buf.extend_from_slice(&outline.bounds.1.to_le_bytes());
    buf.extend_from_slice(&outline.bounds.2.to_le_bytes());
    buf.extend_from_slice(&outline.bounds.3.to_le_bytes());
    buf
}

/// Deserialize a GlyphOutline from compact binary format.
pub fn deserialize_outline(data: &[u8]) -> Option<GlyphOutline> {
    let mut pos = 0;
    let read_u32 = |pos: &mut usize| -> Option<u32> {
        if *pos + 4 > data.len() {
            return None;
        }
        let v = u32::from_le_bytes(data[*pos..*pos + 4].try_into().ok()?);
        *pos += 4;
        Some(v)
    };
    let read_f64 = |pos: &mut usize| -> Option<f64> {
        if *pos + 8 > data.len() {
            return None;
        }
        let v = f64::from_le_bytes(data[*pos..*pos + 8].try_into().ok()?);
        *pos += 8;
        Some(v)
    };
    let read_i16 = |pos: &mut usize| -> Option<i16> {
        if *pos + 2 > data.len() {
            return None;
        }
        let v = i16::from_le_bytes(data[*pos..*pos + 2].try_into().ok()?);
        *pos += 2;
        Some(v)
    };

    let contour_count = read_u32(&mut pos)? as usize;
    if contour_count > 1000 {
        return None;
    }

    let mut contours = Vec::with_capacity(contour_count);
    for _ in 0..contour_count {
        let point_count = read_u32(&mut pos)? as usize;
        if point_count > 10000 {
            return None;
        }
        let mut points = Vec::with_capacity(point_count);
        for _ in 0..point_count {
            let x = read_f64(&mut pos)?;
            let y = read_f64(&mut pos)?;
            if pos >= data.len() {
                return None;
            }
            let on_curve = data[pos] != 0;
            pos += 1;
            points.push(OutlinePoint { x, y, on_curve });
        }
        contours.push(Contour { points });
    }

    let x_min = read_i16(&mut pos)?;
    let y_min = read_i16(&mut pos)?;
    let x_max = read_i16(&mut pos)?;
    let y_max = read_i16(&mut pos)?;

    Some(GlyphOutline {
        contours,
        bounds: (x_min, y_min, x_max, y_max),
        stem_hints: StemHints::default(),
    })
}

// ---------------------------------------------------------------------------
// Minimal CharProc interpreter
// ---------------------------------------------------------------------------

/// Point: (x, y, on_curve).
type Pt = (f64, f64, bool);

struct CharProcInterp {
    stack: Vec<f64>,
    contours: Vec<Vec<Pt>>,
    current: Vec<Pt>,
    x: f64,
    y: f64,
}

impl CharProcInterp {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            contours: Vec::new(),
            current: Vec::new(),
            x: 0.0,
            y: 0.0,
        }
    }

    fn run(&mut self, data: &[u8]) {
        // Simple tokenizer: numbers go on stack, operators trigger actions.
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            // Skip whitespace.
            if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
                i += 1;
                continue;
            }
            // Skip comments.
            if b == b'%' {
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            // Number (including negative and decimal).
            if b == b'-' || b == b'+' || b == b'.' || b.is_ascii_digit() {
                let start = i;
                i += 1;
                while i < data.len()
                    && (data[i].is_ascii_digit()
                        || data[i] == b'.'
                        || data[i] == b'e'
                        || data[i] == b'E'
                        || ((data[i] == b'-' || data[i] == b'+')
                            && i > start
                            && (data[i - 1] == b'e' || data[i - 1] == b'E')))
                {
                    i += 1;
                }
                if let Ok(s) = std::str::from_utf8(&data[start..i]) {
                    if let Ok(n) = s.parse::<f64>() {
                        self.stack.push(n);
                    }
                }
                continue;
            }
            // Operator (alphabetic token).
            if b.is_ascii_alphabetic() || b == b'*' || b == b'\'' {
                let start = i;
                while i < data.len()
                    && (data[i].is_ascii_alphabetic() || data[i] == b'*' || data[i] == b'\'')
                {
                    i += 1;
                }
                self.dispatch(&data[start..i]);
                continue;
            }
            i += 1;
        }
        // Finalize any open contour.
        self.finish_contour();
    }

    fn pop(&mut self) -> f64 {
        self.stack.pop().unwrap_or(0.0)
    }

    fn finish_contour(&mut self) {
        if self.current.len() >= 2 {
            self.contours.push(std::mem::take(&mut self.current));
        } else {
            self.current.clear();
        }
    }

    fn dispatch(&mut self, op: &[u8]) {
        match op {
            // Path construction
            b"m" => {
                self.finish_contour();
                let y = self.pop();
                let x = self.pop();
                self.x = x;
                self.y = y;
                self.current.push((x, y, true));
            }
            b"l" => {
                let y = self.pop();
                let x = self.pop();
                self.x = x;
                self.y = y;
                self.current.push((x, y, true));
            }
            b"c" => {
                let y3 = self.pop();
                let x3 = self.pop();
                let y2 = self.pop();
                let x2 = self.pop();
                let y1 = self.pop();
                let x1 = self.pop();
                self.current.push((x1, y1, false));
                self.current.push((x2, y2, false));
                self.current.push((x3, y3, true));
                self.x = x3;
                self.y = y3;
            }
            b"v" => {
                // Current point is first control point.
                let y3 = self.pop();
                let x3 = self.pop();
                let y2 = self.pop();
                let x2 = self.pop();
                self.current.push((self.x, self.y, false));
                self.current.push((x2, y2, false));
                self.current.push((x3, y3, true));
                self.x = x3;
                self.y = y3;
            }
            b"y" => {
                // End point equals last control point.
                let y3 = self.pop();
                let x3 = self.pop();
                let y1 = self.pop();
                let x1 = self.pop();
                self.current.push((x1, y1, false));
                self.current.push((x3, y3, false));
                self.current.push((x3, y3, true));
                self.x = x3;
                self.y = y3;
            }
            b"re" => {
                let h = self.pop();
                let w = self.pop();
                let y = self.pop();
                let x = self.pop();
                self.finish_contour();
                self.contours.push(vec![
                    (x, y, true),
                    (x + w, y, true),
                    (x + w, y + h, true),
                    (x, y + h, true),
                ]);
                self.x = x;
                self.y = y;
            }
            b"h" => {
                // Close path - contour will be closed by the rasterizer.
                self.finish_contour();
            }
            // Path painting - finalize current path.
            b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => {
                self.finish_contour();
            }
            b"n" => {
                // No-op paint, discard path.
                self.current.clear();
            }
            // d0/d1: glyph metrics. Consume operands.
            b"d" => {
                // Could be d0 (2 args) or d1 (6 args). Just clear stack.
                self.stack.clear();
            }
            // Graphics state (ignore for outline extraction).
            b"q" | b"Q" | b"cm" | b"w" | b"J" | b"j" | b"M" | b"ri" | b"i" | b"gs" => {
                self.stack.clear();
            }
            // Color operators (ignore).
            b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" | b"cs" | b"CS" | b"sc" | b"SC" | b"scn"
            | b"SCN" => {
                self.stack.clear();
            }
            // Text operators (ignore - we extract paths, not text).
            b"BT" | b"ET" | b"Tj" | b"TJ" | b"Tf" | b"Td" | b"TD" | b"Tm" | b"T*" => {
                self.stack.clear();
            }
            // Marked content (ignore).
            b"BMC" | b"BDC" | b"EMC" | b"DP" | b"MP" => {
                self.stack.clear();
            }
            _ => {
                // Unknown operator - clear stack to avoid misinterpreting operands.
                self.stack.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_simple_rectangle() {
        // A simple CharProc: set width, draw a rectangle, fill.
        let data = b"750 0 d1 0 0 m 500 0 l 500 700 l 0 700 l h f";
        let outline = extract_charproc_outline(data, [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]).unwrap();
        assert_eq!(outline.contours.len(), 1);
        assert_eq!(outline.contours[0].points.len(), 4);
        // All points should be on-curve (straight lines).
        assert!(outline.contours[0].points.iter().all(|p| p.on_curve));
    }

    #[test]
    fn extract_rect_operator() {
        let data = b"750 0 d1 100 200 300 400 re f";
        let outline = extract_charproc_outline(data, [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]).unwrap();
        assert_eq!(outline.contours.len(), 1);
        assert_eq!(outline.contours[0].points.len(), 4);
    }

    #[test]
    fn extract_with_curves() {
        let data = b"750 0 d1 0 0 m 100 200 300 400 500 600 c h f";
        let outline = extract_charproc_outline(data, [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]).unwrap();
        assert_eq!(outline.contours.len(), 1);
        // moveto (on) + 2 control points (off) + endpoint (on) = 4
        assert_eq!(outline.contours[0].points.len(), 4);
        assert!(outline.contours[0].points[0].on_curve); // moveto
        assert!(!outline.contours[0].points[1].on_curve); // cp1
        assert!(!outline.contours[0].points[2].on_curve); // cp2
        assert!(outline.contours[0].points[3].on_curve); // endpoint
    }

    #[test]
    fn empty_charproc_returns_none() {
        let data = b"750 0 d1";
        assert!(extract_charproc_outline(data, [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]).is_none());
    }

    #[test]
    fn serialize_roundtrip() {
        let data = b"750 0 d1 0 0 m 500 0 l 500 700 l 0 700 l h f";
        let outline = extract_charproc_outline(data, [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]).unwrap();
        let serialized = serialize_outline(&outline);
        let deserialized = deserialize_outline(&serialized).unwrap();
        assert_eq!(deserialized.contours.len(), outline.contours.len());
        assert_eq!(
            deserialized.contours[0].points.len(),
            outline.contours[0].points.len()
        );
        assert_eq!(deserialized.bounds, outline.bounds);
    }
}
