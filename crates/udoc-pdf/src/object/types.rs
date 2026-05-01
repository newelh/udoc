//! Core PDF object types.
//!
//! Every entity in a PDF is one of these object types. The types here
//! are "owned" — they don't borrow from the source. The lexer produces
//! zero-copy tokens, but the object parser converts them to owned types
//! so objects can live beyond the parse phase.

use std::fmt;

/// Reference to an indirect object: (object number, generation number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    /// Object number.
    pub num: u32,
    /// Generation number.
    pub gen: u16,
}

impl ObjRef {
    /// Create a new object reference.
    #[must_use]
    pub fn new(num: u32, gen: u16) -> Self {
        Self { num, gen }
    }
}

impl fmt::Display for ObjRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.num, self.gen)
    }
}

/// A PDF string, wrapping raw bytes. Can originate from either a literal
/// string `(...)` or a hex string `<...>`.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfString {
    /// The decoded bytes of the string.
    bytes: Vec<u8>,
}

impl PdfString {
    /// Create a new PDF string from raw bytes.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Get the raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Try to interpret the string as UTF-8 text.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }

    /// Convert to owned bytes.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl fmt::Display for PdfString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_text() {
            Some(s) => write!(f, "({s})"),
            None => {
                write!(f, "<")?;
                for b in &self.bytes {
                    write!(f, "{b:02X}")?;
                }
                write!(f, ">")
            }
        }
    }
}

/// An ordered dictionary of PDF name→object mappings.
///
/// Uses a `Vec` of pairs to preserve insertion order (important for
/// some PDF structures). Lookups are O(n) but dictionaries are small
/// enough that this is fine.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfDictionary {
    entries: Vec<(Vec<u8>, PdfObject)>,
}

impl PdfDictionary {
    /// Create an empty dictionary.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Insert a key-value pair. If the key already exists, replace the value.
    pub fn insert(&mut self, key: Vec<u8>, value: PdfObject) {
        for entry in &mut self.entries {
            if entry.0 == key {
                entry.1 = value;
                return;
            }
        }
        self.entries.push((key, value));
    }

    /// Look up a value by key.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&PdfObject> {
        self.entries
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the dictionary is empty.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &PdfObject)> {
        self.entries.iter().map(|(k, v)| (k.as_slice(), v))
    }

    /// Look up a value by key and try to extract it as a boolean.
    #[must_use]
    pub fn get_bool(&self, key: &[u8]) -> Option<bool> {
        self.get(key).and_then(|o| o.as_bool())
    }

    /// Look up a value by key and try to extract it as an integer.
    #[must_use]
    pub fn get_i64(&self, key: &[u8]) -> Option<i64> {
        self.get(key).and_then(|o| o.as_i64())
    }

    /// Look up a value by key and try to extract it as a float.
    /// Integers are promoted to f64.
    #[must_use]
    pub fn get_f64(&self, key: &[u8]) -> Option<f64> {
        self.get(key).and_then(|o| o.as_f64())
    }

    /// Look up a value by key and try to extract it as a name (byte slice).
    #[must_use]
    pub fn get_name(&self, key: &[u8]) -> Option<&[u8]> {
        self.get(key).and_then(|o| o.as_name())
    }

    /// Look up a value by key and try to extract it as a PDF string.
    #[must_use]
    pub fn get_str(&self, key: &[u8]) -> Option<&PdfString> {
        self.get(key).and_then(|o| o.as_pdf_string())
    }

    /// Look up a value by key and try to extract it as an array.
    #[must_use]
    pub fn get_array(&self, key: &[u8]) -> Option<&[PdfObject]> {
        self.get(key).and_then(|o| o.as_array())
    }

    /// Look up a value by key and try to extract it as a dictionary.
    /// Streams are treated as dictionaries (returns the stream's dict).
    /// Does not resolve indirect references; use `ObjectResolver::get_resolved_dict` for that.
    #[must_use]
    pub fn get_dict(&self, key: &[u8]) -> Option<&PdfDictionary> {
        self.get(key).and_then(|o| o.as_dict())
    }

    /// Look up a value by key and try to extract it as an indirect reference.
    #[must_use]
    pub fn get_ref(&self, key: &[u8]) -> Option<ObjRef> {
        self.get(key).and_then(|o| o.as_reference())
    }
}

impl Default for PdfDictionary {
    fn default() -> Self {
        Self::new()
    }
}

impl IntoIterator for PdfDictionary {
    type Item = (Vec<u8>, PdfObject);
    type IntoIter = std::vec::IntoIter<(Vec<u8>, PdfObject)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// A PDF stream: a dictionary plus the byte offset and length of the
/// stream data in the source file.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfStream {
    /// The stream dictionary (contains `/Length`, `/Filter`, etc.).
    pub dict: PdfDictionary,
    /// Byte offset of the stream data.
    ///
    /// After `ObjectParser`: relative to the input slice passed to the parser.
    /// After `ObjectResolver::resolve()`: adjusted to an absolute file offset.
    ///
    /// Code that uses `ObjectParser` directly (e.g., `parse_xref_stream`)
    /// must add the slice's starting position to obtain an absolute offset.
    pub data_offset: u64,
    /// Length of the stream data in bytes (from `/Length` or recovery).
    pub data_length: u64,
}

/// A PDF object.
///
/// This is the central type of the object model. Every entity in a PDF
/// document resolves to one of these variants.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PdfObject {
    /// The null object.
    Null,
    /// Boolean (`true` or `false`).
    Boolean(bool),
    /// Integer number.
    Integer(i64),
    /// Real (floating-point) number.
    Real(f64),
    /// Name object (e.g., `/Type`). Stored as decoded bytes.
    Name(Vec<u8>),
    /// String object (literal or hex).
    String(PdfString),
    /// Array of objects.
    Array(Vec<PdfObject>),
    /// Dictionary of name→object mappings.
    Dictionary(PdfDictionary),
    /// Stream (dictionary + data reference).
    Stream(PdfStream),
    /// Indirect reference to another object.
    Reference(ObjRef),
}

impl PdfObject {
    /// Returns `true` if this is the null object.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn is_null(&self) -> bool {
        matches!(self, PdfObject::Null)
    }

    /// Try to get this object as a boolean.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            PdfObject::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// Try to get this object as an integer.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            PdfObject::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// Try to get this object as a float. Integers are promoted.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PdfObject::Real(n) => Some(*n),
            PdfObject::Integer(n) => Some(*n as f64),
            _ => None,
        }
    }

    /// Try to get this object as a name (byte slice).
    #[must_use]
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            PdfObject::Name(n) => Some(n),
            _ => None,
        }
    }

    /// Try to get this object as a PDF string.
    #[must_use]
    pub fn as_pdf_string(&self) -> Option<&PdfString> {
        match self {
            PdfObject::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get this object as an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[PdfObject]> {
        match self {
            PdfObject::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Try to get this object as a stream.
    #[must_use]
    #[cfg(any(test, feature = "test-internals"))]
    pub fn as_stream(&self) -> Option<&PdfStream> {
        match self {
            PdfObject::Stream(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get this object as a dictionary.
    #[must_use]
    pub fn as_dict(&self) -> Option<&PdfDictionary> {
        match self {
            PdfObject::Dictionary(d) => Some(d),
            PdfObject::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    /// Try to get this object as an indirect reference.
    #[must_use]
    pub fn as_reference(&self) -> Option<ObjRef> {
        match self {
            PdfObject::Reference(r) => Some(*r),
            _ => None,
        }
    }

    /// Return a human-readable name for this object's variant (for error messages).
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            PdfObject::Null => "null",
            PdfObject::Boolean(_) => "boolean",
            PdfObject::Integer(_) => "integer",
            PdfObject::Real(_) => "real",
            PdfObject::Name(_) => "name",
            PdfObject::String(_) => "string",
            PdfObject::Array(_) => "array",
            PdfObject::Dictionary(_) => "dictionary",
            PdfObject::Stream(_) => "stream",
            PdfObject::Reference(_) => "reference",
        }
    }
}

impl fmt::Display for PdfObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfObject::Null => write!(f, "null"),
            PdfObject::Boolean(b) => write!(f, "{b}"),
            PdfObject::Integer(n) => write!(f, "{n}"),
            PdfObject::Real(n) => write!(f, "{n}"),
            PdfObject::Name(n) => {
                write!(f, "/")?;
                for &b in n {
                    if b.is_ascii_graphic() && b != b'#' {
                        write!(f, "{}", b as char)?;
                    } else {
                        write!(f, "#{b:02X}")?;
                    }
                }
                Ok(())
            }
            PdfObject::String(s) => write!(f, "{s}"),
            PdfObject::Array(arr) => {
                write!(f, "[")?;
                for (i, obj) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{obj}")?;
                }
                write!(f, "]")
            }
            PdfObject::Dictionary(dict) => {
                write!(f, "<< ")?;
                for (key, val) in dict.iter() {
                    write!(f, "/")?;
                    for &b in key {
                        write!(f, "{}", b as char)?;
                    }
                    write!(f, " {val} ")?;
                }
                write!(f, ">>")
            }
            PdfObject::Stream(s) => {
                write!(
                    f,
                    "{} stream[{}+{}]",
                    PdfObject::Dictionary(s.dict.clone()),
                    s.data_offset,
                    s.data_length
                )
            }
            PdfObject::Reference(r) => write!(f, "{r}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_obj_ref_display() {
        let r = ObjRef::new(5, 0);
        assert_eq!(format!("{r}"), "5 0 R");
    }

    #[test]
    fn test_pdf_string_text() {
        let s = PdfString::new(b"Hello".to_vec());
        assert_eq!(s.as_text(), Some("Hello"));
    }

    #[test]
    fn test_pdf_string_binary() {
        let s = PdfString::new(vec![0xFF, 0xFE]);
        assert_eq!(s.as_text(), None);
    }

    #[test]
    fn test_pdf_dictionary_insert_get() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Type".to_vec(), PdfObject::Name(b"Catalog".to_vec()));
        assert_eq!(
            dict.get(b"Type"),
            Some(&PdfObject::Name(b"Catalog".to_vec()))
        );
        assert_eq!(dict.get(b"Missing"), None);
    }

    #[test]
    fn test_pdf_dictionary_replace() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Key".to_vec(), PdfObject::Integer(1));
        dict.insert(b"Key".to_vec(), PdfObject::Integer(2));
        assert_eq!(dict.len(), 1);
        assert_eq!(dict.get(b"Key"), Some(&PdfObject::Integer(2)));
    }

    #[test]
    fn test_pdf_object_accessors() {
        assert!(PdfObject::Null.is_null());
        assert_eq!(PdfObject::Boolean(true).as_bool(), Some(true));
        assert_eq!(PdfObject::Integer(42).as_i64(), Some(42));
        assert_eq!(PdfObject::Real(2.5).as_f64(), Some(2.5));
        assert_eq!(PdfObject::Integer(5).as_f64(), Some(5.0));
        assert_eq!(
            PdfObject::Name(b"Type".to_vec()).as_name(),
            Some(b"Type".as_slice())
        );

        let r = ObjRef::new(1, 0);
        assert_eq!(PdfObject::Reference(r).as_reference(), Some(r));
    }

    #[test]
    fn test_pdf_dictionary_typed_accessors() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Bool".to_vec(), PdfObject::Boolean(true));
        dict.insert(b"Int".to_vec(), PdfObject::Integer(42));
        dict.insert(b"Real".to_vec(), PdfObject::Real(2.5));
        dict.insert(b"Name".to_vec(), PdfObject::Name(b"Catalog".to_vec()));
        dict.insert(
            b"Str".to_vec(),
            PdfObject::String(PdfString::new(b"hello".to_vec())),
        );
        dict.insert(
            b"Arr".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(1), PdfObject::Integer(2)]),
        );
        let mut inner = PdfDictionary::new();
        inner.insert(b"X".to_vec(), PdfObject::Integer(10));
        dict.insert(b"Dict".to_vec(), PdfObject::Dictionary(inner));
        dict.insert(b"Ref".to_vec(), PdfObject::Reference(ObjRef::new(5, 0)));

        assert_eq!(dict.get_bool(b"Bool"), Some(true));
        assert_eq!(dict.get_i64(b"Int"), Some(42));
        assert_eq!(dict.get_f64(b"Real"), Some(2.5));
        // Integer promotion to f64
        assert_eq!(dict.get_f64(b"Int"), Some(42.0));
        assert_eq!(dict.get_name(b"Name"), Some(b"Catalog".as_slice()));
        assert_eq!(dict.get_str(b"Str").unwrap().as_bytes(), b"hello");
        assert_eq!(dict.get_array(b"Arr").unwrap().len(), 2);
        assert_eq!(dict.get_dict(b"Dict").unwrap().get_i64(b"X"), Some(10));
        assert_eq!(dict.get_ref(b"Ref"), Some(ObjRef::new(5, 0)));

        // Missing keys return None
        assert_eq!(dict.get_bool(b"Missing"), None);
        assert_eq!(dict.get_i64(b"Missing"), None);
        // Type mismatch returns None
        assert_eq!(dict.get_bool(b"Int"), None);
        assert_eq!(dict.get_i64(b"Bool"), None);
    }

    #[test]
    fn test_pdf_object_display() {
        assert_eq!(format!("{}", PdfObject::Null), "null");
        assert_eq!(format!("{}", PdfObject::Boolean(true)), "true");
        assert_eq!(format!("{}", PdfObject::Integer(42)), "42");
        assert_eq!(format!("{}", PdfObject::Name(b"Type".to_vec())), "/Type");
    }

    #[test]
    fn test_pdf_string_into_bytes() {
        let s = PdfString::new(b"hello".to_vec());
        assert_eq!(s.into_bytes(), b"hello");
    }

    #[test]
    fn test_pdf_string_display_text() {
        let s = PdfString::new(b"hello".to_vec());
        assert_eq!(format!("{s}"), "(hello)");
    }

    #[test]
    fn test_pdf_string_display_binary() {
        let s = PdfString::new(vec![0xFF, 0xFE, 0x00]);
        let display = format!("{s}");
        assert_eq!(display, "<FFFE00>");
    }

    #[test]
    fn test_pdf_object_type_name() {
        assert_eq!(PdfObject::Null.type_name(), "null");
        assert_eq!(PdfObject::Boolean(true).type_name(), "boolean");
        assert_eq!(PdfObject::Integer(0).type_name(), "integer");
        assert_eq!(PdfObject::Real(0.0).type_name(), "real");
        assert_eq!(PdfObject::Name(vec![]).type_name(), "name");
        assert_eq!(
            PdfObject::String(PdfString::new(vec![])).type_name(),
            "string"
        );
        assert_eq!(PdfObject::Array(vec![]).type_name(), "array");
        assert_eq!(
            PdfObject::Dictionary(PdfDictionary::new()).type_name(),
            "dictionary"
        );
        assert_eq!(
            PdfObject::Stream(PdfStream {
                dict: PdfDictionary::new(),
                data_offset: 0,
                data_length: 0,
            })
            .type_name(),
            "stream"
        );
        assert_eq!(
            PdfObject::Reference(ObjRef::new(1, 0)).type_name(),
            "reference"
        );
    }

    #[test]
    fn test_pdf_object_display_real() {
        assert_eq!(format!("{}", PdfObject::Real(2.5)), "2.5");
    }

    #[test]
    fn test_pdf_object_display_name_with_non_ascii() {
        let name = PdfObject::Name(vec![b'A', 0x00, b'#']);
        let s = format!("{name}");
        assert_eq!(s, "/A#00#23");
    }

    #[test]
    fn test_pdf_object_display_string() {
        let s = PdfObject::String(PdfString::new(b"test".to_vec()));
        assert_eq!(format!("{s}"), "(test)");
    }

    #[test]
    fn test_pdf_object_display_array() {
        let arr = PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Integer(2),
            PdfObject::Boolean(true),
        ]);
        assert_eq!(format!("{arr}"), "[1 2 true]");
    }

    #[test]
    fn test_pdf_object_display_empty_array() {
        assert_eq!(format!("{}", PdfObject::Array(vec![])), "[]");
    }

    #[test]
    fn test_pdf_object_display_dictionary() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Type".to_vec(), PdfObject::Name(b"Catalog".to_vec()));
        let obj = PdfObject::Dictionary(dict);
        let s = format!("{obj}");
        assert!(s.contains("<<"), "got: {s}");
        assert!(s.contains("/Type"), "got: {s}");
        assert!(s.contains("/Catalog"), "got: {s}");
        assert!(s.contains(">>"), "got: {s}");
    }

    #[test]
    fn test_pdf_object_display_stream() {
        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: 100,
            data_length: 50,
        };
        let obj = PdfObject::Stream(stream);
        let s = format!("{obj}");
        assert!(s.contains("stream[100+50]"), "got: {s}");
    }

    #[test]
    fn test_pdf_object_display_reference() {
        let r = PdfObject::Reference(ObjRef::new(5, 2));
        assert_eq!(format!("{r}"), "5 2 R");
    }

    #[test]
    fn test_accessor_type_mismatches() {
        // as_bool on non-bool
        assert_eq!(PdfObject::Integer(1).as_bool(), None);
        assert_eq!(PdfObject::Null.as_bool(), None);

        // as_i64 on non-int
        assert_eq!(PdfObject::Boolean(true).as_i64(), None);
        assert_eq!(PdfObject::Real(1.0).as_i64(), None);

        // as_f64 on non-numeric
        assert_eq!(PdfObject::Null.as_f64(), None);
        assert_eq!(PdfObject::Name(vec![]).as_f64(), None);

        // as_name on non-name
        assert_eq!(PdfObject::Integer(1).as_name(), None);
        assert_eq!(PdfObject::Null.as_name(), None);

        // as_pdf_string on non-string
        assert!(PdfObject::Integer(1).as_pdf_string().is_none());

        // as_array on non-array
        assert!(PdfObject::Integer(1).as_array().is_none());
        assert!(PdfObject::Null.as_array().is_none());

        // as_stream on non-stream
        assert!(PdfObject::Integer(1).as_stream().is_none());
        assert!(PdfObject::Dictionary(PdfDictionary::new())
            .as_stream()
            .is_none());

        // as_dict returns stream's dict
        let stream = PdfStream {
            dict: PdfDictionary::new(),
            data_offset: 0,
            data_length: 0,
        };
        assert!(PdfObject::Stream(stream).as_dict().is_some());
        // as_dict on non-dict, non-stream
        assert!(PdfObject::Integer(1).as_dict().is_none());

        // as_reference on non-ref
        assert_eq!(PdfObject::Integer(1).as_reference(), None);
    }

    #[test]
    fn test_dict_typed_accessor_type_mismatches() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Int".to_vec(), PdfObject::Integer(42));
        dict.insert(b"Bool".to_vec(), PdfObject::Boolean(true));
        dict.insert(b"Name".to_vec(), PdfObject::Name(b"X".to_vec()));

        // Wrong type access
        assert_eq!(dict.get_f64(b"Bool"), None);
        assert_eq!(dict.get_name(b"Int"), None);
        assert_eq!(dict.get_str(b"Int"), None);
        assert_eq!(dict.get_array(b"Int"), None);
        assert_eq!(dict.get_dict(b"Int"), None);
        assert_eq!(dict.get_ref(b"Int"), None);
    }
}
