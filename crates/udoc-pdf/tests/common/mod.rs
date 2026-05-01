//! Shared test utilities for building hand-crafted PDFs.
//!
//! `PdfBuilder` tracks object offsets and generates valid xref tables,
//! eliminating boilerplate in test helpers that construct PDFs from scratch.

#![allow(dead_code)]

use std::io::Write;

/// Tracks object offsets for building xref tables.
pub struct PdfBuilder {
    pub buf: Vec<u8>,
    objects: Vec<(u32, u64)>, // (obj_num, byte_offset)
}

impl PdfBuilder {
    pub fn new(version: &str) -> Self {
        let mut buf = Vec::new();
        writeln!(buf, "%PDF-{}", version).unwrap();
        // Binary comment (high bytes signal binary PDF, per spec)
        buf.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
        PdfBuilder {
            buf,
            objects: Vec::new(),
        }
    }

    /// Register an object at the current buffer offset without writing
    /// standard object framing. Used when manually writing object bytes
    /// (e.g., streams with intentionally wrong /Length).
    pub fn register_object_offset(&mut self, obj_num: u32) {
        let offset = self.buf.len() as u64;
        self.objects.push((obj_num, offset));
    }

    pub fn add_object(&mut self, obj_num: u32, body: &[u8]) {
        let offset = self.buf.len() as u64;
        self.objects.push((obj_num, offset));
        writeln!(self.buf, "{} 0 obj", obj_num).unwrap();
        self.buf.extend_from_slice(body);
        self.buf.extend_from_slice(b"\nendobj\n");
    }

    pub fn add_stream_object(&mut self, obj_num: u32, dict_extra: &str, data: &[u8]) {
        let offset = self.buf.len() as u64;
        self.objects.push((obj_num, offset));
        write!(
            self.buf,
            "{} 0 obj\n<< /Length {} {} >>\nstream\n",
            obj_num,
            data.len(),
            dict_extra
        )
        .unwrap();
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    pub fn finish(mut self, root_obj: u32) -> Vec<u8> {
        let trailer = format!("/Size {} /Root {} 0 R", self.xref_size(), root_obj);
        self.write_xref_and_trailer(&trailer)
    }

    pub fn finish_with_trailer(mut self, trailer_extra: &str, root_obj: u32) -> Vec<u8> {
        let trailer = format!(
            "/Size {} /Root {} 0 R {}",
            self.xref_size(),
            root_obj,
            trailer_extra
        );
        self.write_xref_and_trailer(&trailer)
    }

    fn xref_size(&self) -> u32 {
        self.objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1
    }

    fn write_xref_and_trailer(&mut self, trailer_entries: &str) -> Vec<u8> {
        let xref_offset = self.buf.len();
        let size = self.xref_size();

        write!(self.buf, "xref\n0 {}\n", size).unwrap();

        let mut offsets = vec![None; size as usize];
        for &(num, off) in &self.objects {
            offsets[num as usize] = Some(off);
        }

        write!(self.buf, "0000000000 65535 f \r\n").unwrap();
        for entry in offsets.iter().skip(1) {
            if let Some(off) = entry {
                write!(self.buf, "{:010} 00000 n \r\n", off).unwrap();
            } else {
                write!(self.buf, "0000000000 00000 f \r\n").unwrap();
            }
        }

        write!(self.buf, "trailer\n<< {} >>\n", trailer_entries).unwrap();
        write!(self.buf, "startxref\n{}\n%%EOF\n", xref_offset).unwrap();

        std::mem::take(&mut self.buf)
    }
}
