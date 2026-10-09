//! The language image (`.lsl`, ISSUES M12): a forged [`Language`] as bytes.
//!
//! An image is the forged tables themselves — kinds, lexer, parser tables,
//! fields, supertypes, injections — so loading one skips forging entirely.
//! It is deterministic: the same sketch forged by the same lang-forge gives
//! byte-identical images on every platform and every run (LSF2 §5).
//!
//! # Layout
//!
//! ```text
//! magic     4 bytes   "LSL\0"
//! format    u16 LE    IMAGE_FORMAT (1 for lang-forge 2.0.0-alpha.1)
//! reserved  u16 LE    0
//! length    u64 LE    length of the body
//! hash      u64 LE    FNV-1a 64 of the body
//! body                the tables, little-endian, length-prefixed slices
//! ```
//!
//! # Untrusted images
//!
//! An image may come from anywhere, so loading one assumes nothing: the
//! header, length, and hash are checked first; every slice's length is
//! checked against the bytes left before anything is allocated; every
//! string is checked to be UTF-8; and once decoded, every index the lexer
//! and parser follow at run time — expression, item, rule, set, kind, mode,
//! class, level, automaton row — is checked to be in range, so a language
//! loaded from any bytes that pass parses without panicking. A damaged or
//! foreign image is an [`ImageError`], never a crash.
//!
//! Forge-time warnings ([`Language::warnings`]) are not stored.

use alloc::{boxed::Box, vec::Vec};
use core::fmt;

use syntax_lang::Span;

use crate::{Language, kind::Kind};

/// The image format this lang-forge writes and reads.
///
/// It changes whenever the image layout does; an image of another format is
/// refused with [`ImageError::Format`]. Re-forge the sketch to get an image
/// in the current format.
///
/// # Examples
///
/// ```
/// use lang_forge::{IMAGE_FORMAT, Language};
///
/// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nx = \"NUMBER\"\n")?;
/// let image = lang.to_image();
/// assert_eq!(u16::from_le_bytes([image[4], image[5]]), IMAGE_FORMAT);
/// # Ok::<(), lang_forge::Error>(())
/// ```
pub const IMAGE_FORMAT: u16 = 1;

const MAGIC: &[u8; 4] = b"LSL\0";
const HEADER: usize = 4 + 2 + 2 + 8 + 8;

/// Why bytes could not be loaded as a language image.
///
/// # Examples
///
/// ```
/// use lang_forge::{ImageError, Language};
///
/// assert_eq!(Language::from_image(b"not an image").unwrap_err(), ImageError::NotAnImage);
///
/// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nx = \"NUMBER\"\n")?;
/// let mut image = lang.to_image();
/// let last = image.len() - 1;
/// image[last] ^= 0xFF;
/// assert_eq!(Language::from_image(&image).unwrap_err(), ImageError::Corrupt);
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageError {
    /// The bytes do not start with the image header.
    NotAnImage,
    /// The image is of a format this lang-forge does not read. Re-forge the
    /// sketch.
    Format(u16),
    /// The image is truncated, or its body does not match its hash.
    Corrupt,
    /// The image's hash matches, but its tables are not ones lang-forge
    /// builds: an index out of range, a malformed automaton, or invalid
    /// text. It was not written by this lang-forge.
    Invalid,
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnImage => f.write_str("not a lang-forge language image"),
            Self::Format(v) => write!(
                f,
                "language image format {v} is not {IMAGE_FORMAT}, the format this lang-forge reads; forge the sketch again"
            ),
            Self::Corrupt => f.write_str("the language image is truncated or damaged"),
            Self::Invalid => {
                f.write_str("the language image holds tables lang-forge does not build")
            }
        }
    }
}

impl core::error::Error for ImageError {}

/// FNV-1a, 64 bits: a fixed, platform-independent hash of the body.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Appends little-endian values.
#[derive(Default)]
pub(crate) struct Writer {
    pub(crate) out: Vec<u8>,
}

/// Reads little-endian values, failing on anything out of range.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

/// The result of a decode step.
pub(crate) type Res<T> = Result<T, ImageError>;

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Res<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(ImageError::Invalid)?;
        let slice = self.bytes.get(self.pos..end).ok_or(ImageError::Invalid)?;
        self.pos = end;
        Ok(slice)
    }

    /// The number of bytes left.
    pub(crate) fn left(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// A slice length, checked against the bytes left (each item takes at
    /// least `min` bytes), so no decode allocates more than the input allows.
    pub(crate) fn len(&mut self, min: usize) -> Res<usize> {
        let n = u32::get(self)? as usize;
        if n.saturating_mul(min.max(1)) > self.left() {
            return Err(ImageError::Invalid);
        }
        Ok(n)
    }

    pub(crate) fn done(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

/// A value that can be written to and read from an image.
pub(crate) trait Image: Sized {
    /// The fewest bytes one value takes, for allocation checks.
    const MIN: usize = 1;
    fn put(&self, w: &mut Writer);
    fn get(r: &mut Reader<'_>) -> Res<Self>;
}

macro_rules! int_image {
    ($($t:ty),*) => {$(
        impl Image for $t {
            const MIN: usize = core::mem::size_of::<$t>();
            fn put(&self, w: &mut Writer) {
                w.out.extend_from_slice(&self.to_le_bytes());
            }
            fn get(r: &mut Reader<'_>) -> Res<Self> {
                let bytes = r.take(core::mem::size_of::<$t>())?;
                let mut buf = [0u8; core::mem::size_of::<$t>()];
                buf.copy_from_slice(bytes);
                Ok(<$t>::from_le_bytes(buf))
            }
        }
    )*};
}
int_image!(u8, u16, u32, u64, i8);

impl Image for bool {
    fn put(&self, w: &mut Writer) {
        w.out.push(u8::from(*self));
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        match u8::get(r)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ImageError::Invalid),
        }
    }
}

impl Image for char {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) {
        (*self as u32).put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        char::from_u32(u32::get(r)?).ok_or(ImageError::Invalid)
    }
}

impl Image for Kind {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) {
        self.bits().put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok(Kind::from_bits(u32::get(r)?))
    }
}

impl Image for Span {
    const MIN: usize = 8;
    fn put(&self, w: &mut Writer) {
        self.start().to_u32().put(w);
        self.end().to_u32().put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        let (start, end) = (u32::get(r)?, u32::get(r)?);
        if start > end {
            return Err(ImageError::Invalid);
        }
        Ok(Span::new(start, end))
    }
}

impl Image for Box<str> {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) {
        (self.len() as u32).put(w);
        w.out.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        let n = r.len(1)?;
        let bytes = r.take(n)?;
        core::str::from_utf8(bytes)
            .map(Box::from)
            .map_err(|_| ImageError::Invalid)
    }
}

impl<T: Image> Image for Box<[T]> {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) {
        (self.len() as u32).put(w);
        for item in self.iter() {
            item.put(w);
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok(Vec::<T>::get(r)?.into())
    }
}

impl<T: Image> Image for Vec<T> {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) {
        (self.len() as u32).put(w);
        for item in self {
            item.put(w);
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        let n = r.len(T::MIN)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(T::get(r)?);
        }
        Ok(out)
    }
}

impl<T: Image> Image for Option<T> {
    fn put(&self, w: &mut Writer) {
        match self {
            None => 0u8.put(w),
            Some(v) => {
                1u8.put(w);
                v.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        match u8::get(r)? {
            0 => Ok(None),
            1 => Ok(Some(T::get(r)?)),
            _ => Err(ImageError::Invalid),
        }
    }
}

impl<A: Image, B: Image> Image for (A, B) {
    const MIN: usize = A::MIN + B::MIN;
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
        self.1.put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

impl<A: Image, B: Image, C: Image> Image for (A, B, C) {
    const MIN: usize = A::MIN + B::MIN + C::MIN;
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
        self.1.put(w);
        self.2.put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok((A::get(r)?, B::get(r)?, C::get(r)?))
    }
}

impl<T: Image + Copy + Default, const N: usize> Image for [T; N] {
    const MIN: usize = T::MIN * N;
    fn put(&self, w: &mut Writer) {
        for item in self {
            item.put(w);
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        let mut out = [T::default(); N];
        for slot in &mut out {
            *slot = T::get(r)?;
        }
        Ok(out)
    }
}

impl<T: Image> Image for Box<T> {
    const MIN: usize = T::MIN;
    fn put(&self, w: &mut Writer) {
        (**self).put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok(Box::new(T::get(r)?))
    }
}

impl Language {
    /// Writes the forged language as a `.lsl` image.
    ///
    /// Loading the image with [`from_image`](Self::from_image) gives a
    /// language that lexes and parses exactly as this one, without forging
    /// the sketch again (Packaged mode, fast startup). The bytes are
    /// deterministic: the same sketch forged by the same lang-forge writes
    /// the same image on every platform. Forge-time
    /// [`warnings`](Self::warnings) are not part of the image.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"sum\"\n[rules]\nsum = \"NUMBER ('+' NUMBER)*\"\n",
    /// )?;
    /// let image = lang.to_image();
    /// assert_eq!(&image[..4], b"LSL\0");
    ///
    /// let loaded = Language::from_image(&image).expect("a valid image");
    /// assert_eq!(loaded.parse("1 + 2").dump(), lang.parse("1 + 2").dump());
    /// assert_eq!(loaded.to_image(), image);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn to_image(&self) -> Vec<u8> {
        let mut body = Writer::default();
        self.tables().put(&mut body);
        let body = body.out;
        let mut out = Vec::with_capacity(HEADER + body.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&IMAGE_FORMAT.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(body.len() as u64).to_le_bytes());
        out.extend_from_slice(&fnv1a(&body).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Loads a language from a `.lsl` image written by
    /// [`to_image`](Self::to_image).
    ///
    /// The image is treated as untrusted: it is checked completely before
    /// it is used (see the module documentation), so any bytes either load
    /// as a working language or are refused.
    ///
    /// # Errors
    ///
    /// [`ImageError::NotAnImage`] for bytes without the image header,
    /// [`ImageError::Format`] for an image of another format,
    /// [`ImageError::Corrupt`] for a truncated or damaged one, and
    /// [`ImageError::Invalid`] for tables lang-forge does not build.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"n\"\n[rules]\nn = \"NUMBER+\"\n")?;
    /// let loaded = Language::from_image(&lang.to_image()).expect("valid");
    /// assert_eq!(loaded.name(), "n");
    /// assert!(!loaded.parse("1 2 3").has_errors());
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn from_image(bytes: &[u8]) -> Result<Language, ImageError> {
        if bytes.len() < HEADER || &bytes[..4] != MAGIC {
            return Err(ImageError::NotAnImage);
        }
        let format = u16::from_le_bytes([bytes[4], bytes[5]]);
        if format != IMAGE_FORMAT {
            return Err(ImageError::Format(format));
        }
        let mut word = [0u8; 8];
        word.copy_from_slice(&bytes[8..16]);
        let length = u64::from_le_bytes(word);
        word.copy_from_slice(&bytes[16..24]);
        let hash = u64::from_le_bytes(word);
        let body = &bytes[HEADER..];
        if bytes[6..8] != [0, 0] || body.len() as u64 != length || fnv1a(body) != hash {
            return Err(ImageError::Corrupt);
        }
        let mut r = Reader::new(body);
        let grammar = crate::grammar::Grammar::get(&mut r)?;
        if !r.done() {
            return Err(ImageError::Invalid);
        }
        grammar.validate()?;
        Ok(Language::from_grammar(grammar))
    }
}

/// Fails with [`ImageError::Invalid`] unless `ok`.
pub(crate) fn check(ok: bool) -> Res<()> {
    if ok { Ok(()) } else { Err(ImageError::Invalid) }
}

impl Image for usize {
    const MIN: usize = 8;
    fn put(&self, w: &mut Writer) {
        (*self as u64).put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        usize::try_from(u64::get(r)?).map_err(|_| ImageError::Invalid)
    }
}

/// Implements [`Image`] for a struct field by field, in declaration order.
macro_rules! image_struct {
    ($t:ident { $($f:ident),* $(,)? }) => {
        impl crate::image::Image for $t {
            fn put(&self, w: &mut crate::image::Writer) {
                $( crate::image::Image::put(&self.$f, w); )*
            }
            fn get(r: &mut crate::image::Reader<'_>) -> crate::image::Res<Self> {
                Ok(Self { $( $f: crate::image::Image::get(r)?, )* })
            }
        }
    };
}
pub(crate) use image_struct;

/// Implements [`Image`] for a field-less enum, each variant with its tag.
macro_rules! image_enum {
    ($t:ident { $($v:ident = $n:literal),* $(,)? }) => {
        impl crate::image::Image for $t {
            fn put(&self, w: &mut crate::image::Writer) {
                let tag: u8 = match self { $( $t::$v => $n, )* };
                crate::image::Image::put(&tag, w);
            }
            fn get(r: &mut crate::image::Reader<'_>) -> crate::image::Res<Self> {
                match <u8 as crate::image::Image>::get(r)? {
                    $( $n => Ok($t::$v), )*
                    _ => Err(crate::image::ImageError::Invalid),
                }
            }
        }
    };
}
pub(crate) use image_enum;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn test_primitives_round_trip_and_bounds() {
        let mut w = Writer::default();
        (7u8, 300u16, 70_000u32).put(&mut w);
        Some(Box::<str>::from("é")).put(&mut w);
        Vec::from([1u16, 2, 3]).put(&mut w);
        true.put(&mut w);
        let mut r = Reader::new(&w.out);
        assert_eq!(<(u8, u16, u32)>::get(&mut r), Ok((7, 300, 70_000)));
        assert_eq!(Option::<Box<str>>::get(&mut r), Ok(Some(Box::from("é"))));
        assert_eq!(Vec::<u16>::get(&mut r), Ok(Vec::from([1, 2, 3])));
        assert_eq!(bool::get(&mut r), Ok(true));
        assert!(r.done());
        // A length larger than the input is refused before allocating.
        let mut r = Reader::new(&[0xFF, 0xFF, 0xFF, 0x7F]);
        assert_eq!(Vec::<u64>::get(&mut r), Err(ImageError::Invalid));
        // Bad booleans, bad UTF-8, bad chars.
        assert_eq!(bool::get(&mut Reader::new(&[2])), Err(ImageError::Invalid));
        assert_eq!(
            Box::<str>::get(&mut Reader::new(&[1, 0, 0, 0, 0xFF])),
            Err(ImageError::Invalid)
        );
        assert_eq!(
            char::get(&mut Reader::new(&[0, 0xD8, 0, 0])),
            Err(ImageError::Invalid)
        );
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    }
}
