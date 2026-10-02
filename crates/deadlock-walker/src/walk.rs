//! Reflective walk of a live C++ protobuf object into protobuf wire bytes.
//!
//! # Layout assumptions
//!
//! Generated protobuf-cpp 3.x classes, MSVC x64:
//!
//! * `+0` vtable pointer, `+8` `_internal_metadata_`. Everything after is described by a
//!   [`crate::layout::MessageLayout`]; nothing here assumes field order.
//! * Presence is a `uint32[]` bitmap at `has_bits_offset`. The bit index of each singular
//!   field is part of its layout: protoc assigns bits in memory-layout order, not field
//!   order, and gives repeated fields none.
//! * Scalars are stored inline at their native width (`bool` is one byte, enums are
//!   `int32`).
//! * `string` / `bytes` are an `ArenaStringPtr`: one pointer to a `std::string` with the low
//!   two bits used as tags. Null means empty. MSVC `std::string` is 32 bytes: a union of
//!   a 16-byte inline buffer and a heap pointer, then `size` at `+16` and `capacity` at
//!   `+24`; data is inline when `capacity < 16`.
//! * A singular message is a pointer to the sub-object, null when absent.
//! * `RepeatedField<T>` is 16 bytes: `int size`, `int capacity`, then a pointer straight to
//!   element 0 (the arena pointer sits in the 8 bytes before it, which the walker never
//!   reads). Measured on the live client: the pointer of a 27-element `uint32` list points
//!   at `1, 2, 3, ...`.
//! * `RepeatedPtrField<T>` is 24 bytes: arena pointer, `int size`, `int capacity`, `Rep*`;
//!   the `Rep` is an `int allocated_size` padded to 8, then `T*` elements. For strings `T` is
//!   a plain `std::string`, not an `ArenaStringPtr`.
//!
//! All of these are properties of the protobuf runtime version the game links, which is
//! why they are collected in [`Abi`] rather than scattered as constants.

use deadlock_memory::mem::MemoryReader;

use crate::error::{Error, Result};
use crate::layout::LayoutSource;
use crate::schema::{FieldKind, Scalar, Schema};

/// Runtime-version-dependent container layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abi {
    /// `RepeatedField`: offset of the element-count `int`.
    pub repeated_size_at: u64,
    /// `RepeatedField`: offset of the element pointer.
    pub repeated_rep_at: u64,
    /// `RepeatedField`: offset of element 0 from that pointer.
    pub repeated_elems_at: u64,
    /// `RepeatedPtrField`: offset of the element-count `int`.
    pub ptr_repeated_size_at: u64,
    /// `RepeatedPtrField`: offset of the `Rep*`.
    pub ptr_repeated_rep_at: u64,
    /// `RepeatedPtrField`: offset of element 0 from the `Rep*`.
    pub ptr_repeated_elems_at: u64,
    /// Bits to clear from an `ArenaStringPtr` to get the `std::string*`.
    pub string_tag_mask: u64,
}

impl Abi {
    /// protobuf-cpp 3.21 built with MSVC for x64, as measured on the live client.
    pub const PROTOBUF3_MSVC_X64: Abi = Abi {
        repeated_size_at: 0,
        repeated_rep_at: 8,
        repeated_elems_at: 0,
        ptr_repeated_size_at: 8,
        ptr_repeated_rep_at: 16,
        ptr_repeated_elems_at: 8,
        string_tag_mask: 0x3,
    };
}

/// Bounds on what a walk will accept from the target.
///
/// Every count and pointer comes from memory that may be mid-free, so each is capped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Deepest message nesting.
    pub max_depth: usize,
    /// Largest repeated field.
    pub max_elements: usize,
    /// Most bytes of output.
    pub max_output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 32,
            max_elements: 1 << 20,
            max_output: 64 * 1024 * 1024,
        }
    }
}

/// Reads live protobuf objects out of a target process.
pub struct Walker<'a> {
    mem: &'a dyn MemoryReader,
    schema: &'a Schema,
    layouts: &'a dyn LayoutSource,
    abi: Abi,
    limits: Limits,
}

impl<'a> Walker<'a> {
    /// A walker with the default [`Abi`] and [`Limits`].
    pub fn new(
        mem: &'a dyn MemoryReader,
        schema: &'a Schema,
        layouts: &'a dyn LayoutSource,
    ) -> Self {
        Walker {
            mem,
            schema,
            layouts,
            abi: Abi::PROTOBUF3_MSVC_X64,
            limits: Limits::default(),
        }
    }

    /// Override the container layouts.
    pub fn with_abi(mut self, abi: Abi) -> Self {
        self.abi = abi;
        self
    }

    /// Override the safety limits.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Serialise the `message` object at `addr` to protobuf wire bytes, fields in number
    /// order.
    pub fn serialize(&self, message: &str, addr: u64) -> Result<Vec<u8>> {
        let mut produced = 0;
        self.message(message, addr, 0, &mut produced)
    }

    /// Like [`Walker::serialize`], but fails with [`crate::Error::Changed`] unless the
    /// object's vtable is `vtable` both before and after the read. A freed object has
    /// its first qword overwritten by the allocator, so this is how a mid-walk free shows.
    pub fn serialize_checked(&self, message: &str, addr: u64, vtable: u64) -> Result<Vec<u8>> {
        if self.mem.read_u64(addr)? != vtable {
            return Err(Error::Changed);
        }
        let wire = self.serialize(message, addr)?;
        if self.mem.read_u64(addr)? != vtable {
            return Err(Error::Changed);
        }
        Ok(wire)
    }

    fn message(
        &self,
        name: &str,
        addr: u64,
        depth: usize,
        produced: &mut usize,
    ) -> Result<Vec<u8>> {
        if depth >= self.limits.max_depth {
            return Err(Error::Limit("message nesting"));
        }
        let schema = self
            .schema
            .message(name)
            .ok_or_else(|| Error::UnknownMessage(name.to_string()))?;
        let layout = self
            .layouts
            .layout(name)
            .ok_or_else(|| Error::NoLayout(name.to_string()))?;
        let obj = self.mem.read_bytes(addr, layout.size)?;

        let mut fields: Vec<_> = schema.fields.iter().collect();
        fields.sort_by_key(|f| f.number);

        let mut out = Vec::new();
        for f in fields {
            let Some(fl) = layout.fields.get(&f.number) else {
                continue;
            };
            let at = fl.offset as usize;
            let outside = || {
                Error::NoLayout(format!(
                    "{name}: field {} lies outside the object",
                    f.number
                ))
            };
            match (&f.kind, f.repeated) {
                (FieldKind::Unsupported(what), _) => {
                    return Err(Error::Unsupported {
                        message: name.to_string(),
                        field: f.number,
                        what,
                    });
                }
                (FieldKind::Scalar(s), false) => {
                    let raw = obj.get(at..at + s.size()).ok_or_else(outside)?;
                    if self.present(&obj, layout.has_bits_offset, fl.has_bit, || {
                        raw.iter().any(|&b| b != 0)
                    })? {
                        put_scalar(&mut out, f.number, *s, raw);
                    }
                }
                (FieldKind::Scalar(s), true) => {
                    let header = obj.get(at..at + 16).ok_or_else(outside)?;
                    let Some((count, rep)) = self.repeated_header(
                        header,
                        self.abi.repeated_size_at,
                        self.abi.repeated_rep_at,
                    )?
                    else {
                        continue;
                    };
                    let width = s.size();
                    let raw = self
                        .mem
                        .read_bytes(rep + self.abi.repeated_elems_at, count * width)?;
                    if f.packed {
                        let mut body = Vec::new();
                        for item in raw.chunks_exact(width) {
                            put_scalar_value(&mut body, *s, item);
                        }
                        put_tag(&mut out, f.number, 2);
                        put_varint(&mut out, body.len() as u64);
                        out.extend_from_slice(&body);
                    } else {
                        for item in raw.chunks_exact(width) {
                            put_scalar(&mut out, f.number, *s, item);
                        }
                    }
                }
                (FieldKind::String | FieldKind::Bytes, false) => {
                    let raw = obj.get(at..at + 8).ok_or_else(outside)?;
                    let tagged = u64::from_le_bytes(raw.try_into().unwrap());
                    let ptr = tagged & !self.abi.string_tag_mask;
                    if self.present(&obj, layout.has_bits_offset, fl.has_bit, || ptr != 0)? {
                        let bytes = if ptr == 0 {
                            Vec::new()
                        } else {
                            self.std_string(ptr)?
                        };
                        self.put_bytes(&mut out, f.number, &bytes, produced)?;
                    }
                }
                (FieldKind::Message(sub), false) => {
                    let raw = obj.get(at..at + 8).ok_or_else(outside)?;
                    let ptr = u64::from_le_bytes(raw.try_into().unwrap());
                    if ptr != 0
                        && self.present(&obj, layout.has_bits_offset, fl.has_bit, || true)?
                    {
                        let body = self.message(sub, ptr, depth + 1, produced)?;
                        self.put_bytes(&mut out, f.number, &body, produced)?;
                    }
                }
                (kind, true) => {
                    let header = obj.get(at..at + 24).ok_or_else(outside)?;
                    let Some((count, rep)) = self.repeated_header(
                        header,
                        self.abi.ptr_repeated_size_at,
                        self.abi.ptr_repeated_rep_at,
                    )?
                    else {
                        continue;
                    };
                    let ptrs = self
                        .mem
                        .read_bytes(rep + self.abi.ptr_repeated_elems_at, count * 8)?;
                    for p in ptrs.as_chunks::<8>().0 {
                        let ptr = u64::from_le_bytes(*p);
                        let body = match kind {
                            FieldKind::Message(sub) => {
                                self.message(sub, ptr, depth + 1, produced)?
                            }
                            _ => self.std_string(ptr)?,
                        };
                        self.put_bytes(&mut out, f.number, &body, produced)?;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Presence of a singular field. Without a has-bit (proto3-style or oneof) the field
    /// counts as present when `nonzero` says it holds something.
    fn present(
        &self,
        obj: &[u8],
        has_bits_offset: u32,
        bit: Option<u32>,
        nonzero: impl FnOnce() -> bool,
    ) -> Result<bool> {
        let Some(bit) = bit else {
            return Ok(nonzero());
        };
        let at = has_bits_offset as usize + (bit / 32) as usize * 4;
        let word = obj.get(at..at + 4).ok_or(Error::NoLayout(
            "has-bits lie outside the object".to_string(),
        ))?;
        Ok(u32::from_le_bytes(word.try_into().unwrap()) >> (bit % 32) & 1 != 0)
    }

    /// Element count and `Rep*` of a `RepeatedField` or `RepeatedPtrField`, `None` if empty.
    fn repeated_header(
        &self,
        header: &[u8],
        size_at: u64,
        rep_at: u64,
    ) -> Result<Option<(usize, u64)>> {
        let size_at = size_at as usize;
        let rep_at = rep_at as usize;
        let count = i32::from_le_bytes(header[size_at..size_at + 4].try_into().unwrap());
        let rep = u64::from_le_bytes(header[rep_at..rep_at + 8].try_into().unwrap());
        let count = usize::try_from(count).map_err(|_| Error::Limit("negative repeated count"))?;
        if count > self.limits.max_elements {
            return Err(Error::Limit("repeated field length"));
        }
        Ok((count > 0 && rep != 0).then_some((count, rep)))
    }

    /// Contents of an MSVC x64 `std::string` at `addr`.
    fn std_string(&self, addr: u64) -> Result<Vec<u8>> {
        let raw = self.mem.read_bytes(addr, 32)?;
        let word = |at: usize| u64::from_le_bytes(raw[at..at + 8].try_into().unwrap());
        let (size, capacity) = (word(16), word(24));
        if size > capacity || size > crate::walk::MAX_STRING {
            return Err(Error::Changed);
        }
        let size = size as usize;
        if capacity < 16 {
            Ok(raw[..size].to_vec())
        } else {
            Ok(self.mem.read_bytes(word(0), size)?)
        }
    }

    fn put_bytes(
        &self,
        out: &mut Vec<u8>,
        number: u32,
        bytes: &[u8],
        produced: &mut usize,
    ) -> Result<()> {
        *produced += bytes.len();
        if *produced > self.limits.max_output {
            return Err(Error::Limit("output size"));
        }
        put_tag(out, number, 2);
        put_varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
        Ok(())
    }
}

const MAX_STRING: u64 = 16 * 1024 * 1024;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_tag(out: &mut Vec<u8>, number: u32, wire_type: u8) {
    put_varint(out, u64::from(number) << 3 | u64::from(wire_type));
}

fn wire_type(s: Scalar) -> u8 {
    match s {
        Scalar::Fixed64 | Scalar::SFixed64 | Scalar::Double => 1,
        Scalar::Fixed32 | Scalar::SFixed32 | Scalar::Float => 5,
        _ => 0,
    }
}

/// A tagged scalar from its in-memory bytes.
fn put_scalar(out: &mut Vec<u8>, number: u32, s: Scalar, raw: &[u8]) {
    put_tag(out, number, wire_type(s));
    put_scalar_value(out, s, raw);
}

/// An untagged scalar from its in-memory bytes, as it appears in a packed field.
fn put_scalar_value(out: &mut Vec<u8>, s: Scalar, raw: &[u8]) {
    let u32_of = || u32::from_le_bytes(raw[..4].try_into().unwrap());
    let u64_of = || u64::from_le_bytes(raw[..8].try_into().unwrap());
    match s {
        Scalar::Bool => put_varint(out, u64::from(raw[0] != 0)),
        // Negative values are sign-extended to ten bytes on the wire.
        Scalar::Int32 | Scalar::Enum => put_varint(out, i64::from(u32_of() as i32) as u64),
        Scalar::UInt32 => put_varint(out, u64::from(u32_of())),
        Scalar::SInt32 => {
            let v = u32_of() as i32;
            put_varint(out, u64::from(((v << 1) ^ (v >> 31)) as u32));
        }
        Scalar::Int64 | Scalar::UInt64 => put_varint(out, u64_of()),
        Scalar::SInt64 => {
            let v = u64_of() as i64;
            put_varint(out, ((v << 1) ^ (v >> 63)) as u64);
        }
        Scalar::Fixed32 | Scalar::SFixed32 | Scalar::Float => out.extend_from_slice(&raw[..4]),
        Scalar::Fixed64 | Scalar::SFixed64 | Scalar::Double => out.extend_from_slice(&raw[..8]),
    }
}
