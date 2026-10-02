//! Where each field lives inside a C++ message object.
//!
//! This is the half of the problem the descriptors cannot answer. Offsets differ per game
//! build, so they are an input: derived from the client's own compiled tables (live) or
//! declared by a test.

use std::collections::{BTreeMap, HashMap};

/// Where one field is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldLayout {
    /// Byte offset of the field from the start of the object.
    pub offset: u32,
    /// Index into the `_has_bits_` bitmap. `None` for repeated fields.
    pub has_bit: Option<u32>,
}

/// Layout of one message class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageLayout {
    /// `sizeof` the generated class. The walker reads the object with one bulk read.
    pub size: usize,
    /// Byte offset of the `_has_bits_` array (`uint32[]`).
    pub has_bits_offset: u32,
    /// Per-field storage, by field number.
    pub fields: BTreeMap<u32, FieldLayout>,
}

/// Supplies [`MessageLayout`]s by message full name.
pub trait LayoutSource {
    /// Layout of `message`, or `None` if unknown.
    fn layout(&self, message: &str) -> Option<&MessageLayout>;
}

impl LayoutSource for HashMap<String, MessageLayout> {
    fn layout(&self, message: &str) -> Option<&MessageLayout> {
        self.get(message)
    }
}
