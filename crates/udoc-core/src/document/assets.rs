//! Asset store for heavy document assets (images, fonts).
//!
//! The [`AssetStore`] holds binary blobs (images, font programs) that are
//! referenced by index from the content spine. It is separate from
//! [`Document`](super::Document) so callers can detach it for lightweight
//! serialization or pass it independently to output formatters.
//!
//! Images are referenced via [`AssetRef<ImageAsset>`]. The type alias
//! [`ImageRef`](super::ImageRef) is provided for ergonomic use in the
//! content spine (`Block::Image`, `Inline::InlineImage`).

use std::marker::PhantomData;

use crate::image::{ImageFilter, PageImage};

/// A store for heavy document assets (images, fonts).
/// Detachable from Document for lightweight serialization.
#[derive(Debug, Default, Clone)]
pub struct AssetStore {
    images: Vec<ImageAsset>,
    fonts: Vec<FontAsset>,
}

/// Typed reference to an asset in the store.
///
/// Cheap to copy -- just a usize index with a phantom type tag.
/// `Copy` is implemented manually because `PhantomData<T>` only derives
/// `Copy` when `T: Copy`, and our asset types (`ImageAsset`, `FontAsset`)
/// are not `Copy`.
#[derive(Debug, PartialEq, Eq)]
pub struct AssetRef<T> {
    index: usize,
    _phantom: PhantomData<T>,
}

impl<T> Clone for AssetRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for AssetRef<T> {}

impl<T> AssetRef<T> {
    /// Create a new asset reference by index.
    pub fn new(index: usize) -> Self {
        Self {
            index,
            _phantom: PhantomData,
        }
    }

    /// The index into the asset store.
    pub fn index(&self) -> usize {
        self.index
    }
}

#[cfg(feature = "serde")]
impl<T> serde::Serialize for AssetRef<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("index", &self.index)?;
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de, T> serde::Deserialize<'de> for AssetRef<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct AssetRefVisitor<U>(PhantomData<U>);

        impl<'de, U> Visitor<'de> for AssetRefVisitor<U> {
            type Value = AssetRef<U>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an asset reference with index")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut index: Option<usize> = None;
                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "index" => index = Some(access.next_value()?),
                        _ => {
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                let index = index.ok_or_else(|| serde::de::Error::missing_field("index"))?;
                Ok(AssetRef::new(index))
            }
        }

        deserializer.deserialize_map(AssetRefVisitor(PhantomData))
    }
}

/// Image asset stored in the [`AssetStore`].
///
/// **JSON round-trip note:** When serialized to JSON, `data` is replaced
/// with `data_length` to avoid multi-megabyte JSON arrays. After a JSON
/// round-trip, `data` will be empty. Use `--images` to extract actual
/// image bytes to files.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImageAsset {
    /// Raw image data (compressed or raw depending on filter).
    pub data: Vec<u8>,
    /// The encoding of the image data.
    pub filter: ImageFilter,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Bits per color component.
    pub bits_per_component: u8,
}

impl ImageAsset {
    /// Create a new ImageAsset.
    pub fn new(
        data: Vec<u8>,
        filter: ImageFilter,
        width: u32,
        height: u32,
        bits_per_component: u8,
    ) -> Self {
        Self {
            data,
            filter,
            width,
            height,
            bits_per_component,
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for ImageAsset {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(5))?;
        // Serialize byte count instead of raw bytes to keep JSON reasonable.
        map.serialize_entry("data_length", &self.data.len())?;
        map.serialize_entry("filter", &self.filter)?;
        map.serialize_entry("width", &self.width)?;
        map.serialize_entry("height", &self.height)?;
        map.serialize_entry("bits_per_component", &self.bits_per_component)?;
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ImageAsset {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct ImageAssetVisitor;

        impl<'de> Visitor<'de> for ImageAssetVisitor {
            type Value = ImageAsset;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an image asset object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut data: Option<Vec<u8>> = None;
                let mut filter: Option<ImageFilter> = None;
                let mut width: Option<u32> = None;
                let mut height: Option<u32> = None;
                let mut bits_per_component: Option<u8> = None;

                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "data" => data = Some(access.next_value()?),
                        "data_length" => {
                            // Accept but ignore the length; data is empty when
                            // deserialized from JSON (use --images for actual bytes).
                            let _: usize = access.next_value()?;
                        }
                        "filter" => filter = Some(access.next_value()?),
                        "width" => width = Some(access.next_value()?),
                        "height" => height = Some(access.next_value()?),
                        "bits_per_component" => bits_per_component = Some(access.next_value()?),
                        _ => {
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                Ok(ImageAsset {
                    data: data.unwrap_or_default(),
                    filter: filter.ok_or_else(|| serde::de::Error::missing_field("filter"))?,
                    width: width.ok_or_else(|| serde::de::Error::missing_field("width"))?,
                    height: height.ok_or_else(|| serde::de::Error::missing_field("height"))?,
                    bits_per_component: bits_per_component
                        .ok_or_else(|| serde::de::Error::missing_field("bits_per_component"))?,
                })
            }
        }

        deserializer.deserialize_map(ImageAssetVisitor)
    }
}

/// Convert a [`PageImage`] into [`ImageAsset`], discarding the positional
/// `bbox` field. The `bbox` is stored separately in the presentation overlay
/// and is not part of the asset store.
impl From<PageImage> for ImageAsset {
    fn from(img: PageImage) -> Self {
        Self {
            data: img.data,
            filter: img.filter,
            width: img.width,
            height: img.height,
            bits_per_component: img.bits_per_component,
        }
    }
}

/// Convert a reference to a [`PageImage`] into [`ImageAsset`] by cloning the
/// image data. The `bbox` field is discarded.
impl From<&PageImage> for ImageAsset {
    fn from(img: &PageImage) -> Self {
        Self {
            data: img.data.clone(),
            filter: img.filter,
            width: img.width,
            height: img.height,
            bits_per_component: img.bits_per_component,
        }
    }
}

/// Font program asset for rendering.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FontAsset {
    /// Font name (PostScript name or family name).
    pub name: String,
    /// Raw font program data.
    pub data: Vec<u8>,
    /// Type of font program.
    pub program_type: FontProgramType,
    /// PDF encoding map: byte code -> glyph name. Used by the renderer
    /// for by-code glyph lookup in subset fonts with custom encodings.
    /// None for fonts without encoding data or non-PDF backends.
    pub encoding_map: Option<Vec<(u8, String)>>,
    /// Parsed `/W` advance-width table for composite (Type0) fonts.
    ///
    /// `(default_width, entries)` where entries are explicit `(cid, width)`
    /// pairs from the `/W` array, widths in glyph-space units (1/1000 em).
    /// The renderer prefers these PDF-declared widths over embedded `hmtx`
    /// entries when resolving per-glyph advances for CID TrueType subsets
    /// (see issue #182: MS Word export PDFs whose /W disagrees with hmtx).
    ///
    /// None for non-composite fonts, or composite fonts with no /W data.
    pub cid_widths: Option<(u32, Vec<(u32, f64)>)>,
}

impl FontAsset {
    /// Create a new FontAsset.
    pub fn new(name: String, data: Vec<u8>, program_type: FontProgramType) -> Self {
        Self {
            name,
            data,
            program_type,
            encoding_map: None,
            cid_widths: None,
        }
    }

    /// Create a FontAsset with an encoding map.
    pub fn with_encoding(
        name: String,
        data: Vec<u8>,
        program_type: FontProgramType,
        encoding_map: Option<Vec<(u8, String)>>,
    ) -> Self {
        Self {
            name,
            data,
            program_type,
            encoding_map,
            cid_widths: None,
        }
    }

    /// Attach parsed `/W` data to this font asset.
    ///
    /// Consumed by the renderer's `advance_width_by_gid` path for CID
    /// TrueType subsets (see issue #182).
    pub fn with_cid_widths(mut self, cid_widths: Option<(u32, Vec<(u32, f64)>)>) -> Self {
        self.cid_widths = cid_widths;
        self
    }
}

/// Type of font program embedded in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum FontProgramType {
    TrueType,
    Cff,
    Type1,
    /// Type3 glyph outline (serialized from CharProc path data).
    /// Asset name: "type3:{font_name}:U+{hex_codepoint}".
    Type3,
}

/// Controls which asset types are extracted.
///
/// Marked `#[non_exhaustive]` so adding new asset categories (e.g. ICC
/// color profiles, shading patterns) post-alpha is not a breaking change
/// for callers using struct-update syntax. Construct via [`AssetConfig::default`]
/// then chain field setters, or via the [`AssetConfig::none`] /
/// [`AssetConfig::images_only`] / [`AssetConfig::fonts_only`] /
/// [`AssetConfig::all`] presets.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AssetConfig {
    /// Whether to extract images.
    pub images: bool,
    /// Whether to extract fonts.
    pub fonts: bool,
    /// When true, extraction aborts with
    /// [`crate::error::Error::font_fallback_required`] on the first
    /// non-Exact [`crate::text::FontResolution`] (any
    /// [`crate::text::FontResolution::Substituted`] or
    /// [`crate::text::FontResolution::SyntheticFallback`]).
    ///
    /// Default `false`: the PDF backend emits a `FallbackFontSubstitution`
    /// warning and continues extraction with the substituted font. Opt in
    /// when production workflows need a hard guarantee that text
    /// extraction reflects the document's exact embedded fonts.
    ///
    /// Currently observed by the PDF backend only. Other backends decode
    /// text via the source document's own Unicode strings and never
    /// invoke a font-substitution step, so this flag is a no-op there.
    pub strict_fonts: bool,
}

impl AssetConfig {
    /// No asset extraction.
    pub fn none() -> Self {
        Self {
            images: false,
            fonts: false,
            strict_fonts: false,
        }
    }

    /// Extract images only (default behavior).
    pub fn images_only() -> Self {
        Self {
            images: true,
            fonts: false,
            strict_fonts: false,
        }
    }

    /// Extract fonts only.
    pub fn fonts_only() -> Self {
        Self {
            images: false,
            fonts: true,
            strict_fonts: false,
        }
    }

    /// Extract all asset types.
    pub fn all() -> Self {
        Self {
            images: true,
            fonts: true,
            strict_fonts: false,
        }
    }

    /// Builder: set whether to extract images.
    ///
    /// ```
    /// use udoc_core::document::AssetConfig;
    /// let cfg = AssetConfig::default().images(false);
    /// assert!(!cfg.images);
    /// ```
    pub fn images(mut self, on: bool) -> Self {
        self.images = on;
        self
    }

    /// Builder: set whether to extract fonts.
    ///
    /// ```
    /// use udoc_core::document::AssetConfig;
    /// let cfg = AssetConfig::default().fonts(true);
    /// assert!(cfg.fonts);
    /// ```
    pub fn fonts(mut self, on: bool) -> Self {
        self.fonts = on;
        self
    }

    /// Builder: set the strict-font-resolution mode.
    ///
    /// See the [`AssetConfig::strict_fonts`] field for semantics.
    ///
    /// ```
    /// use udoc_core::document::AssetConfig;
    /// let cfg = AssetConfig::default().strict_fonts(true);
    /// assert!(cfg.strict_fonts);
    /// ```
    pub fn strict_fonts(mut self, on: bool) -> Self {
        self.strict_fonts = on;
        self
    }
}

impl Default for AssetConfig {
    fn default() -> Self {
        // Backward compat: images enabled, fonts disabled.
        Self::images_only()
    }
}

impl AssetStore {
    /// Create an empty asset store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an image asset. Returns a typed reference to it.
    pub fn add_image(&mut self, image: ImageAsset) -> AssetRef<ImageAsset> {
        let index = self.images.len();
        self.images.push(image);
        AssetRef::new(index)
    }

    /// Add a font asset. Returns a typed reference to it.
    pub fn add_font(&mut self, font: FontAsset) -> AssetRef<FontAsset> {
        let index = self.fonts.len();
        self.fonts.push(font);
        AssetRef::new(index)
    }

    /// Look up an image asset by reference.
    pub fn image(&self, r: AssetRef<ImageAsset>) -> Option<&ImageAsset> {
        self.images.get(r.index())
    }

    /// Look up a font asset by reference.
    pub fn font(&self, r: AssetRef<FontAsset>) -> Option<&FontAsset> {
        self.fonts.get(r.index())
    }

    /// Find a font by name (first match).
    pub fn font_by_name(&self, name: &str) -> Option<&FontAsset> {
        self.fonts.iter().find(|f| f.name == name)
    }

    /// All image assets.
    pub fn images(&self) -> &[ImageAsset] {
        &self.images
    }

    /// All font assets.
    pub fn fonts(&self) -> &[FontAsset] {
        &self.fonts
    }

    /// Whether the store contains no assets at all.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.fonts.is_empty()
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for AssetStore {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("images", &self.images)?;
        map.serialize_entry("fonts", &self.fonts)?;
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for AssetStore {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct AssetStoreVisitor;

        impl<'de> Visitor<'de> for AssetStoreVisitor {
            type Value = AssetStore;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an asset store with images and fonts")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut images: Option<Vec<ImageAsset>> = None;
                let mut fonts: Option<Vec<FontAsset>> = None;

                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "images" => images = Some(access.next_value()?),
                        "fonts" => fonts = Some(access.next_value()?),
                        _ => {
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                Ok(AssetStore {
                    images: images.unwrap_or_default(),
                    fonts: fonts.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_map(AssetStoreVisitor)
    }
}

// FontAsset needs manual serde since it's non_exhaustive
#[cfg(feature = "serde")]
impl serde::Serialize for FontAsset {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("data_length", &self.data.len())?;
        map.serialize_entry("program_type", &self.program_type)?;
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for FontAsset {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct FontAssetVisitor;

        impl<'de> Visitor<'de> for FontAssetVisitor {
            type Value = FontAsset;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a font asset object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut name: Option<String> = None;
                let mut data: Option<Vec<u8>> = None;
                let mut program_type: Option<FontProgramType> = None;

                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "name" => name = Some(access.next_value()?),
                        "data" => data = Some(access.next_value()?),
                        "data_length" => {
                            let _: usize = access.next_value()?;
                        }
                        "program_type" => program_type = Some(access.next_value()?),
                        _ => {
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                Ok(FontAsset {
                    name: name.ok_or_else(|| serde::de::Error::missing_field("name"))?,
                    data: data.unwrap_or_default(),
                    program_type: program_type
                        .ok_or_else(|| serde::de::Error::missing_field("program_type"))?,
                    encoding_map: None,
                    cid_widths: None,
                })
            }
        }

        deserializer.deserialize_map(FontAssetVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageFilter;

    #[test]
    fn asset_store_add_image() {
        let mut store = AssetStore::new();
        let r = store.add_image(ImageAsset::new(
            vec![0xFF, 0xD8],
            ImageFilter::Jpeg,
            100,
            100,
            8,
        ));
        assert_eq!(r.index(), 0);
        assert_eq!(store.images().len(), 1);
        assert_eq!(store.image(r).unwrap().width, 100);
    }

    #[test]
    fn asset_store_add_font() {
        let mut store = AssetStore::new();
        let r = store.add_font(FontAsset {
            name: "Arial".into(),
            data: vec![0x00, 0x01],
            program_type: FontProgramType::TrueType,
            encoding_map: None,
            cid_widths: None,
        });
        assert_eq!(r.index(), 0);
        assert_eq!(store.fonts().len(), 1);
        assert_eq!(store.font(r).unwrap().name, "Arial");
    }

    #[test]
    fn asset_store_font_by_name() {
        let mut store = AssetStore::new();
        store.add_font(FontAsset {
            name: "Helvetica".into(),
            data: vec![],
            program_type: FontProgramType::Cff,
            encoding_map: None,
            cid_widths: None,
        });
        assert!(store.font_by_name("Helvetica").is_some());
        assert!(store.font_by_name("Times").is_none());
    }

    #[test]
    fn asset_store_is_empty() {
        let store = AssetStore::new();
        assert!(store.is_empty());

        let mut store2 = AssetStore::new();
        store2.add_image(ImageAsset::new(vec![], ImageFilter::Raw, 1, 1, 8));
        assert!(!store2.is_empty());
    }

    #[test]
    fn asset_ref_index() {
        let r: AssetRef<ImageAsset> = AssetRef::new(42);
        assert_eq!(r.index(), 42);
    }

    #[test]
    fn image_asset_from_page_image() {
        use crate::geometry::BoundingBox;

        let page_img = PageImage::new(
            vec![0xFF, 0xD8],
            ImageFilter::Jpeg,
            640,
            480,
            8,
            Some(BoundingBox::new(0.0, 0.0, 100.0, 75.0)),
        );
        let asset = ImageAsset::from(page_img);
        assert_eq!(asset.data, vec![0xFF, 0xD8]);
        assert_eq!(asset.filter, ImageFilter::Jpeg);
        assert_eq!(asset.width, 640);
        assert_eq!(asset.height, 480);
        assert_eq!(asset.bits_per_component, 8);
    }

    #[test]
    fn image_asset_from_page_image_ref() {
        let page_img = PageImage::new(vec![0x89, 0x50], ImageFilter::Png, 32, 32, 8, None);
        let asset = ImageAsset::from(&page_img);
        assert_eq!(asset.data, vec![0x89, 0x50]);
        assert_eq!(asset.filter, ImageFilter::Png);
        assert_eq!(asset.width, 32);
        // original is still usable after conversion from ref
        assert_eq!(page_img.width, 32);
    }

    #[test]
    fn asset_config_defaults() {
        let config = AssetConfig::default();
        assert!(config.images);
        assert!(!config.fonts);
        assert!(!config.strict_fonts);
    }

    #[test]
    fn asset_config_constructors() {
        let none = AssetConfig::none();
        assert!(!none.images);
        assert!(!none.fonts);

        let all = AssetConfig::all();
        assert!(all.images);
        assert!(all.fonts);

        let imgs = AssetConfig::images_only();
        assert!(imgs.images);
        assert!(!imgs.fonts);

        let fts = AssetConfig::fonts_only();
        assert!(!fts.images);
        assert!(fts.fonts);
    }

    #[test]
    fn asset_config_builder_fonts_setter() {
        let cfg = AssetConfig::default().fonts(true);
        assert!(cfg.fonts);
        let cfg = cfg.fonts(false);
        assert!(!cfg.fonts);
    }

    #[test]
    fn asset_config_builder_images_setter() {
        let cfg = AssetConfig::default().images(false);
        assert!(!cfg.images);
        let cfg = cfg.images(true);
        assert!(cfg.images);
    }

    #[test]
    fn asset_config_builder_strict_fonts_setter() {
        let cfg = AssetConfig::default().strict_fonts(true);
        assert!(cfg.strict_fonts);
        let cfg = cfg.strict_fonts(false);
        assert!(!cfg.strict_fonts);
    }

    #[test]
    fn asset_config_builder_chained() {
        let cfg = AssetConfig::default().fonts(true).strict_fonts(true);
        assert!(cfg.fonts);
        assert!(cfg.images); // default kept
        assert!(cfg.strict_fonts);
    }
}
