//! Message layouts read from the client's own compiled protobuf tables.
//!
//! protobuf-cpp 3.21 emits, for every `.proto` file, these read-only tables plus a
//! `DescriptorTable` that points at them:
//!
//! ```text
//! DescriptorTable (.data)       +4  int size                 +16 const char* filename
//!                               +44 int num_messages         +48 MigrationSchema* schemas
//!                               +56 Message* const* default_instances
//!                               +64 uint32_t* offsets
//! MigrationSchema (16 bytes)    { offsets_index, has_bit_indices_offset,
//!                                 inlined_string_indices_offset, object_size }
//! offsets[] at offsets_index    has_bits, metadata, extensions, oneof_case, weak_fields,
//!                               inlined_string_donated        (6 words)
//!                               then one field offset per field, in declaration order
//! offsets[] at has_bit_indices_offset
//!                               then one has-bit index per field, `!0` for none
//! ```
//!
//! `schemas[i]`, `default_instances[i]` and message `i` of the file are the same message.
//! The default instance's first qword is the class vtable, whose RTTI names the class, so
//! no table entry is matched to a message by position in the descriptor pool alone.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::layout::{FieldLayout, LayoutSource, MessageLayout};
use crate::pe::PeImage;
use crate::rtti::mangled_class_name;
use crate::schema::{FieldKind, FieldSchema, Schema};

const NONE: u32 = !0;
const HEADER_WORDS: u32 = 6;
const MAX_MESSAGES: u32 = 100_000;
const MAX_DEPS: u32 = 4096;

/// A message the tables describe but whose layout was not derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// Full protobuf name.
    pub message: String,
    /// Why the table entry was not used.
    pub reason: String,
}

/// Layouts of every message the client's tables describe.
#[derive(Debug, Default)]
pub struct ClientTables {
    layouts: HashMap<String, MessageLayout>,
    skipped: Vec<Skipped>,
}

impl ClientTables {
    /// Read every `DescriptorTable` in `image` and derive a layout for each message the
    /// `schema` also knows.
    ///
    /// A table entry is used only if the field count it implies equals the descriptor's.
    /// Offsets are positional, so a client newer than the pinned descriptors would
    /// otherwise assign offsets to the wrong fields; those messages land in
    /// [`ClientTables::skipped`] instead.
    ///
    /// # Errors
    ///
    /// [`Error::NoTables`] if the image holds no descriptor table at all.
    pub fn read(image: &PeImage, schema: &Schema) -> Result<Self> {
        let img = Image {
            base: image.base(),
            bytes: image.bytes(),
        };
        let tables = find_descriptor_tables(image, &img);
        if tables.is_empty() {
            return Err(Error::NoTables);
        }
        let by_class: HashMap<String, &str> =
            schema.names().map(|n| (mangled_class_name(n), n)).collect();

        let mut out = ClientTables::default();
        for table in tables.iter().filter(|t| t.messages > 0) {
            for i in 0..table.messages {
                out.read_message(&img, schema, &by_class, table, i);
            }
        }
        Ok(out)
    }

    fn read_message(
        &mut self,
        img: &Image<'_>,
        schema: &Schema,
        by_class: &HashMap<String, &str>,
        table: &Table,
        i: usize,
    ) {
        let Some(instance) = img.u64_at(table.instances + 8 * i) else {
            return;
        };
        let Some(name) = img.class_of(instance).and_then(|c| by_class.get(&c)) else {
            return;
        };
        let name = (*name).to_string();
        if self.layouts.contains_key(&name) {
            return;
        }
        match derive(img, schema, table, i, &name) {
            Ok(layout) => {
                self.layouts.insert(name, layout);
            }
            Err(reason) => self.skipped.push(Skipped {
                message: name,
                reason,
            }),
        }
    }

    /// Entries that were found but not used.
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    /// Number of messages with a layout.
    pub fn len(&self) -> usize {
        self.layouts.len()
    }

    /// Whether no layout was derived.
    pub fn is_empty(&self) -> bool {
        self.layouts.is_empty()
    }
}

impl LayoutSource for ClientTables {
    fn layout(&self, message: &str) -> Option<&MessageLayout> {
        self.layouts.get(message)
    }
}

/// The schema of the client's own build, decoded from the `FileDescriptorProto`s embedded in
/// its descriptor tables.
///
/// The pinned `valveprotos` descriptors can lag the client: a field changes type or a message
/// grows. This is the descriptor set the client's generated code was compiled from.
///
/// # Errors
///
/// [`Error::NoTables`] without descriptor tables, [`Error::BadDescriptor`] if the embedded
/// descriptors do not form a valid pool.
pub fn client_schema(image: &PeImage) -> Result<Schema> {
    let img = Image {
        base: image.base(),
        bytes: image.bytes(),
    };
    let tables = find_descriptor_tables(image, &img);
    if tables.is_empty() {
        return Err(Error::NoTables);
    }
    let by_rva: HashMap<usize, usize> =
        tables.iter().enumerate().map(|(i, t)| (t.rva, i)).collect();

    // `DescriptorPool` wants a file after the files it imports.
    let mut order = Vec::with_capacity(tables.len());
    let mut state = vec![0u8; tables.len()];
    for root in 0..tables.len() {
        let mut stack = vec![(root, 0usize)];
        while let Some((i, next)) = stack.pop() {
            if next == 0 {
                if state[i] != 0 {
                    continue;
                }
                state[i] = 1;
            }
            let t = &tables[i];
            let dep = (next < t.num_deps)
                .then(|| img.u64_at(t.deps + 8 * next))
                .flatten()
                .and_then(|p| img.rva(p))
                .and_then(|rva| by_rva.get(&rva).copied());
            if next < t.num_deps {
                stack.push((i, next + 1));
                if let Some(d) = dep.filter(|&d| state[d] == 0) {
                    stack.push((d, 0));
                }
            } else {
                order.push(i);
            }
        }
    }

    let mut set = Vec::new();
    for &i in &order {
        let t = &tables[i];
        let proto = &img.bytes[t.descriptor..t.descriptor + t.descriptor_len];
        set.push(0x0a);
        put_varint(&mut set, proto.len() as u64);
        set.extend_from_slice(proto);
    }
    let pool = prost_reflect::DescriptorPool::decode(set.as_slice())
        .map_err(|e| Error::BadDescriptor(e.to_string()))?;
    Ok(Schema::from_pool(&pool))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Image<'a> {
    base: u64,
    bytes: &'a [u8],
}

impl Image<'_> {
    fn rva(&self, addr: u64) -> Option<usize> {
        let rva = usize::try_from(addr.checked_sub(self.base)?).ok()?;
        (rva < self.bytes.len()).then_some(rva)
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            self.bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    }

    fn u64_at(&self, at: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            self.bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
        ))
    }

    fn cstr(&self, at: usize, max: usize) -> Option<&[u8]> {
        let window = self.bytes.get(at..)?;
        let window = &window[..window.len().min(max)];
        let end = window.iter().position(|&b| b == 0)?;
        Some(&window[..end])
    }

    /// The decorated RTTI name of the class whose object is at `instance`.
    fn class_of(&self, instance: u64) -> Option<String> {
        let vtable = self.u64_at(self.rva(instance)?)?;
        let col = self.rva(self.u64_at(self.rva(vtable)?.checked_sub(8)?)?)?;
        if self.u32_at(col)? != 1 {
            return None;
        }
        let descriptor = self.u32_at(col + 12)? as usize;
        let name = self.cstr(descriptor + 16, 512)?;
        String::from_utf8(name.to_vec()).ok()
    }
}

/// Where one file's tables are, as RVAs.
struct Table {
    rva: usize,
    name: String,
    descriptor: usize,
    descriptor_len: usize,
    deps: usize,
    num_deps: usize,
    messages: usize,
    schemas: usize,
    instances: usize,
    offsets: usize,
}

fn find_descriptor_tables(image: &PeImage, img: &Image<'_>) -> Vec<Table> {
    let mut found: Vec<Table> = Vec::new();
    for (sec_rva, sec) in image.data_sections() {
        let first = sec_rva.next_multiple_of(8) - sec_rva;
        for off in (first..sec.len().saturating_sub(72)).step_by(8) {
            if let Some(t) = table_at(img, sec_rva + off)
                && !found.iter().any(|f| f.name == t.name)
            {
                found.push(t);
            }
        }
    }
    found
}

/// Whether a `DescriptorTable` starts at `at`. The filename pointer is checked first: it is
/// the cheapest test that almost every non-table position fails. The embedded descriptor
/// must then start with its own `name` field, which says the same file name.
fn table_at(img: &Image<'_>, at: usize) -> Option<Table> {
    let name = img.cstr(img.rva(img.u64_at(at + 16)?)?, 256)?;
    let plausible = name.len() > ".proto".len()
        && name.ends_with(b".proto")
        && name.iter().all(u8::is_ascii_graphic);
    if !plausible || img.bytes[at] > 1 || img.bytes[at + 1] > 1 {
        return None;
    }
    let descriptor = img.rva(img.u64_at(at + 8)?)?;
    let descriptor_len = img.u32_at(at + 4)? as usize;
    let embedded = img
        .bytes
        .get(descriptor..descriptor.checked_add(descriptor_len)?)?;
    let (&tag, rest) = embedded.split_first()?;
    if tag != 0x0a || rest.len() <= name.len() || rest[0] as usize != name.len() {
        return None;
    }
    if &rest[1..=name.len()] != name {
        return None;
    }

    let messages = img.u32_at(at + 44)?;
    if messages > MAX_MESSAGES || img.u32_at(at + 40)? > MAX_DEPS {
        return None;
    }
    let (mut schemas, mut instances, mut offsets) = (0, 0, 0);
    if messages > 0 {
        schemas = img.rva(img.u64_at(at + 48)?)?;
        instances = img.rva(img.u64_at(at + 56)?)?;
        offsets = img.rva(img.u64_at(at + 64)?)?;
        if !schemas.is_multiple_of(4) || !offsets.is_multiple_of(4) || !instances.is_multiple_of(8)
        {
            return None;
        }
        if img.u32_at(schemas)? != 0 {
            return None;
        }
    }
    Some(Table {
        rva: at,
        name: String::from_utf8(name.to_vec()).ok()?,
        descriptor,
        descriptor_len,
        deps: img.rva(img.u64_at(at + 32)?).unwrap_or(0),
        num_deps: img.u32_at(at + 40)? as usize,
        messages: messages as usize,
        schemas,
        instances,
        offsets,
    })
}

fn derive(
    img: &Image<'_>,
    schema: &Schema,
    table: &Table,
    i: usize,
    name: &str,
) -> std::result::Result<MessageLayout, String> {
    let record = |k: usize| -> Option<[u32; 4]> {
        let at = table.schemas + 16 * k;
        Some([
            img.u32_at(at)?,
            img.u32_at(at + 4)?,
            img.u32_at(at + 8)?,
            img.u32_at(at + 12)?,
        ])
    };
    let unreadable = || "table entry lies outside the image".to_string();
    let [start, has_bits_at, _, size] = record(i).ok_or_else(unreadable)?;
    let fields = &schema.message(name).ok_or_else(unreadable)?.fields;

    let implied = if has_bits_at != NONE {
        has_bits_at.checked_sub(start + HEADER_WORDS)
    } else if i + 1 < table.messages {
        let next = record(i + 1).ok_or_else(unreadable)?[0];
        next.checked_sub(start + HEADER_WORDS)
    } else {
        Some(fields.len() as u32)
    };
    if implied != Some(fields.len() as u32) {
        return Err(format!(
            "compiled table has {} fields, descriptor has {}",
            implied.map_or_else(
                || "an inconsistent number of".to_string(),
                |n| n.to_string()
            ),
            fields.len()
        ));
    }

    let word = |index: u32| img.u32_at(table.offsets + 4 * index as usize);
    let has_bits_offset = match word(start).ok_or_else(unreadable)? {
        NONE => 0,
        at => at,
    };
    let mut out = MessageLayout {
        size: size as usize,
        has_bits_offset,
        fields: Default::default(),
    };
    for (k, f) in fields.iter().enumerate() {
        let k = k as u32;
        let offset = word(start + HEADER_WORDS + k).ok_or_else(unreadable)?;
        if offset == NONE {
            continue;
        }
        let has_bit = if has_bits_at == NONE {
            None
        } else {
            Some(word(has_bits_at + k).ok_or_else(unreadable)?).filter(|&b| b != NONE)
        };
        out.fields.insert(f.number, FieldLayout { offset, has_bit });
    }
    fits(&out, fields)?;
    Ok(out)
}

/// Checks the derived offsets against the shapes the descriptor claims. The field count
/// agreeing does not make the descriptor the client's: a field can change type between
/// builds. Misaligned, overlapping, out-of-object or wrongly-presence-tracked fields give
/// that away.
fn fits(layout: &MessageLayout, fields: &[FieldSchema]) -> std::result::Result<(), String> {
    let has_bits = layout.fields.values().any(|f| f.has_bit.is_some());
    let mut spans = Vec::new();
    for f in fields {
        let Some(fl) = layout.fields.get(&f.number) else {
            continue;
        };
        let (width, align) = match (&f.kind, f.repeated) {
            (FieldKind::Unsupported(_), _) => continue,
            (FieldKind::Scalar(_), true) => (16, 8),
            (_, true) => (24, 8),
            (FieldKind::Scalar(s), false) => (s.size(), s.size()),
            _ => (8, 8),
        };
        let offset = fl.offset as usize;
        let tracked = f.repeated == fl.has_bit.is_none() || (!f.repeated && !has_bits);
        if !tracked || !offset.is_multiple_of(align) || offset + width > layout.size {
            return Err(format!(
                "field {} ({}) at offset {offset:#x} does not fit the descriptor's {}{:?}",
                f.number,
                f.name,
                if f.repeated { "repeated " } else { "" },
                f.kind
            ));
        }
        spans.push((offset, offset + width, f.number));
    }
    spans.sort_unstable();
    if let Some(pair) = spans.windows(2).find(|p| p[0].1 > p[1].0) {
        return Err(format!(
            "fields {} and {} overlap in the compiled layout",
            pair[0].2, pair[1].2
        ));
    }
    Ok(())
}
