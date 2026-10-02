//! A fake MSVC x64 process: a PE image carrying RTTI and vtables, and a heap of
//! protobuf-cpp-shaped objects built from protobuf wire bytes.
//!
//! The object builder plays the part of the C++ compiler and protobuf runtime. It places
//! fields in *descending field-number order* with the has-bits first, so a walker that
//! assumed declaration order would read garbage.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};

use deadlock_memory::mock::MockMemory;
use deadlock_walker::schema::{FieldKind, FieldSchema, MessageSchema, Scalar, Schema};
use deadlock_walker::{FieldLayout, MessageLayout};

pub const MODULE_BASE: u64 = 0x7ffa_1000_0000;
pub const MODULE_SIZE: usize = 0x10_0000;
pub const HEAP_BASE: u64 = 0x1f00_0000_0000;
pub const HEAP_SIZE: usize = 24 * 1024 * 1024;

const TEXT_RVA: usize = 0x1000;
const RDATA_RVA: usize = 0x2000;
const RDATA_END: usize = 0xC_0000;
const DATA_RVA: usize = 0xD_0000;
const DATA_END: usize = 0xF_0000;

const SCN_CODE_EXEC_READ: u32 = 0x6000_0020;
const SCN_RDATA: u32 = 0x4000_0040;
const SCN_DATA: u32 = 0xC000_0040;

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn align(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// The decorated name as it appears in a `type_info`. Written independently of the crate
/// under test so the two cannot share a bug.
pub fn decorated(full_name: &str) -> String {
    format!(".?AV{}@@", full_name.replace('.', "_"))
}

pub struct Image {
    bytes: Vec<u8>,
    rdata_next: usize,
    data_next: usize,
}

impl Image {
    fn new() -> Self {
        let mut b = vec![0u8; MODULE_SIZE];
        b[0..2].copy_from_slice(b"MZ");
        let nt = 0x80usize;
        put_u32(&mut b, 0x3c, nt as u32);
        b[nt..nt + 4].copy_from_slice(b"PE\0\0");
        b[nt + 4..nt + 6].copy_from_slice(&0x8664u16.to_le_bytes());
        b[nt + 6..nt + 8].copy_from_slice(&3u16.to_le_bytes());
        let opt_size = 0xf0usize;
        b[nt + 20..nt + 22].copy_from_slice(&(opt_size as u16).to_le_bytes());
        let opt = nt + 24;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        put_u32(&mut b, opt + 56, MODULE_SIZE as u32);
        let mut sh = opt + opt_size;
        for (name, rva, size, flags) in [
            (".text", TEXT_RVA, 0x1000, SCN_CODE_EXEC_READ),
            (".rdata", RDATA_RVA, RDATA_END - RDATA_RVA, SCN_RDATA),
            (".data", DATA_RVA, DATA_END - DATA_RVA, SCN_DATA),
        ] {
            b[sh..sh + name.len()].copy_from_slice(name.as_bytes());
            put_u32(&mut b, sh + 8, size as u32);
            put_u32(&mut b, sh + 12, rva as u32);
            put_u32(&mut b, sh + 36, flags);
            sh += 40;
        }
        for i in 0..0x1000 {
            b[TEXT_RVA + i] = 0xcc;
        }
        Image {
            bytes: b,
            rdata_next: RDATA_RVA,
            data_next: DATA_RVA,
        }
    }

    fn alloc_rdata(&mut self, n: usize) -> usize {
        let at = align(self.rdata_next, 8);
        self.rdata_next = at + n;
        assert!(self.rdata_next <= RDATA_END);
        at
    }

    fn alloc_data(&mut self, n: usize) -> usize {
        let at = align(self.data_next, 16);
        self.data_next = at + n;
        assert!(self.data_next <= DATA_END);
        at
    }

    /// Returns the primary vtable address (absolute).
    fn add_class(&mut self, mangled: &str) -> u64 {
        let td = self.alloc_data(16 + mangled.len() + 1);
        put_u64(&mut self.bytes, td, MODULE_BASE + 0x6_0000);
        self.bytes[td + 16..td + 16 + mangled.len()].copy_from_slice(mangled.as_bytes());

        // A stray reference to the type descriptor that is not a locator.
        let noise = self.alloc_rdata(32);
        put_u32(&mut self.bytes, noise + 12, td as u32);

        // A secondary vtable (complete-object offset 8) comes first and must be ignored.
        let secondary = self.vtable_with_col(td, 8);
        let _ = secondary;
        self.vtable_with_col(td, 0)
    }

    fn vtable_with_col(&mut self, td: usize, offset: u32) -> u64 {
        let col = self.alloc_rdata(24);
        put_u32(&mut self.bytes, col, 1);
        put_u32(&mut self.bytes, col + 4, offset);
        put_u32(&mut self.bytes, col + 12, td as u32);
        put_u32(&mut self.bytes, col + 16, 0x4_1000);
        put_u32(&mut self.bytes, col + 20, col as u32);
        let vt = self.alloc_rdata(8 + 3 * 8);
        put_u64(&mut self.bytes, vt, MODULE_BASE + col as u64);
        for i in 0..3 {
            put_u64(
                &mut self.bytes,
                vt + 8 + i * 8,
                MODULE_BASE + (TEXT_RVA + 0x10 * (i + 1)) as u64,
            );
        }
        MODULE_BASE + vt as u64 + 8
    }
}

pub struct Heap {
    pub bytes: Vec<u8>,
    next: usize,
}

impl Heap {
    fn new() -> Self {
        Heap {
            bytes: vec![0u8; HEAP_SIZE],
            next: 0x100,
        }
    }

    fn alloc(&mut self, size: usize, align_to: usize) -> u64 {
        let at = align(self.next, align_to.max(8));
        self.next = at + size.max(1);
        assert!(self.next <= HEAP_SIZE, "fake heap exhausted");
        HEAP_BASE + at as u64
    }

    fn put(&mut self, addr: u64, data: &[u8]) {
        let at = (addr - HEAP_BASE) as usize;
        self.bytes[at..at + data.len()].copy_from_slice(data);
    }

    fn used(&self) -> usize {
        align(self.next, 4096)
    }
}

enum Value<'a> {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    Bytes(&'a [u8]),
}

fn read_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

fn parse_wire(buf: &[u8]) -> BTreeMap<u32, Vec<Value<'_>>> {
    let mut out: BTreeMap<u32, Vec<Value<'_>>> = BTreeMap::new();
    let mut pos = 0;
    while pos < buf.len() {
        let key = read_varint(buf, &mut pos);
        let number = (key >> 3) as u32;
        let v = match key & 7 {
            0 => Value::Varint(read_varint(buf, &mut pos)),
            1 => {
                let v = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap());
                pos += 8;
                Value::Fixed64(v)
            }
            2 => {
                let len = read_varint(buf, &mut pos) as usize;
                let v = Value::Bytes(&buf[pos..pos + len]);
                pos += len;
                v
            }
            5 => {
                let v = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
                pos += 4;
                Value::Fixed32(v)
            }
            wt => panic!("wire type {wt} in fixture"),
        };
        out.entry(number).or_default().push(v);
    }
    out
}

fn zigzag32(v: u64) -> u32 {
    let v = v as u32;
    (v >> 1) ^ (v & 1).wrapping_neg()
}

fn zigzag64(v: u64) -> u64 {
    (v >> 1) ^ (v & 1).wrapping_neg()
}

/// In-memory bytes of one scalar value, at the width the C++ class stores it.
fn scalar_bytes(s: Scalar, v: &Value<'_>) -> Vec<u8> {
    match (s, v) {
        (Scalar::Bool, Value::Varint(x)) => vec![u8::from(*x != 0)],
        (Scalar::Int32 | Scalar::UInt32 | Scalar::Enum, Value::Varint(x)) => {
            (*x as u32).to_le_bytes().to_vec()
        }
        (Scalar::SInt32, Value::Varint(x)) => zigzag32(*x).to_le_bytes().to_vec(),
        (Scalar::Int64 | Scalar::UInt64, Value::Varint(x)) => x.to_le_bytes().to_vec(),
        (Scalar::SInt64, Value::Varint(x)) => zigzag64(*x).to_le_bytes().to_vec(),
        (Scalar::Float | Scalar::Fixed32 | Scalar::SFixed32, Value::Fixed32(x)) => {
            x.to_le_bytes().to_vec()
        }
        (Scalar::Double | Scalar::Fixed64 | Scalar::SFixed64, Value::Fixed64(x)) => {
            x.to_le_bytes().to_vec()
        }
        _ => panic!("wire value does not match scalar {s:?}"),
    }
}

/// Every value of a repeated scalar, whether it arrived packed or not.
fn scalar_items(s: Scalar, values: &[Value<'_>]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::Bytes(packed) => {
                let mut pos = 0;
                while pos < packed.len() {
                    let item = match s {
                        Scalar::Float | Scalar::Fixed32 | Scalar::SFixed32 => {
                            let x = u32::from_le_bytes(packed[pos..pos + 4].try_into().unwrap());
                            pos += 4;
                            Value::Fixed32(x)
                        }
                        Scalar::Double | Scalar::Fixed64 | Scalar::SFixed64 => {
                            let x = u64::from_le_bytes(packed[pos..pos + 8].try_into().unwrap());
                            pos += 8;
                            Value::Fixed64(x)
                        }
                        _ => Value::Varint(read_varint(packed, &mut pos)),
                    };
                    out.push(scalar_bytes(s, &item));
                }
            }
            other => out.push(scalar_bytes(s, other)),
        }
    }
    out
}

/// One `.proto` file's compiled tables, for [`World::emit_file_tables`].
pub struct FileTables {
    pub name: String,
    /// Serialized `FileDescriptorProto`; a name-only stub when `None`.
    pub descriptor: Option<Vec<u8>>,
    /// Names of files this one imports; each must be in the same emit call.
    pub deps: Vec<String>,
    pub messages: Vec<String>,
}

pub struct World {
    pub mem: MockMemory,
    pub schema: Schema,
    pub layouts: HashMap<String, MessageLayout>,
    pub vtables: HashMap<String, u64>,
    pub image: Image,
    pub heap: Heap,
}

impl World {
    /// Classes and layouts for `root` and every message reachable from it.
    pub fn new(root: &str) -> Self {
        Self::with_roots(&[root])
    }

    /// Classes and layouts for every message reachable from any of `roots`.
    pub fn with_roots(roots: &[&str]) -> Self {
        let schema = Schema::from_valveprotos();
        let mut image = Image::new();
        let mut layouts = HashMap::new();
        let mut vtables = HashMap::new();
        let mut todo: Vec<String> = roots.iter().map(|r| (*r).to_string()).collect();
        let mut seen = HashSet::new();
        while let Some(name) = todo.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let msg = schema.message(&name).unwrap_or_else(|| panic!("no {name}"));
            for f in &msg.fields {
                if let FieldKind::Message(sub) = &f.kind {
                    todo.push(sub.clone());
                }
            }
            vtables.insert(name.clone(), image.add_class(&decorated(&name)));
            layouts.insert(name.clone(), compute_layout(msg));
        }
        let mut w = World {
            mem: MockMemory::new(4242),
            schema,
            layouts,
            vtables,
            image,
            heap: Heap::new(),
        };
        w.commit();
        w
    }

    /// Place a never-freed "default instance" of `name` in the module's `.data`.
    pub fn add_default_instance(&mut self, name: &str) -> u64 {
        let at = self.image.alloc_data(self.layouts[name].size);
        put_u64(&mut self.image.bytes, at, self.vtables[name]);
        self.commit();
        MODULE_BASE + at as u64
    }

    /// Emit the compiled tables protobuf-cpp 3.21 generates per `.proto` file for `files`
    /// (file name, message full names), each with a minimal serialized descriptor holding
    /// only the file name.
    pub fn emit_tables(&mut self, files: &[(&str, Vec<String>)]) {
        let specs: Vec<FileTables> = files
            .iter()
            .map(|(name, messages)| FileTables {
                name: (*name).to_string(),
                descriptor: None,
                deps: Vec::new(),
                messages: messages.clone(),
            })
            .collect();
        self.emit_file_tables(&specs);
    }

    /// Emit, per file: `offsets[]`, `MigrationSchema schemas[]`, the default instances,
    /// `file_default_instances[]`, the serialized descriptor, the `deps[]` array and the
    /// `DescriptorTable` tying them together. Each file's arrays sit back to back with the
    /// next file's, as a linker lays them out.
    pub fn emit_file_tables(&mut self, files: &[FileTables]) {
        let tables: Vec<usize> = files.iter().map(|_| self.image.alloc_data(96)).collect();
        let abs = |rva: usize| MODULE_BASE + rva as u64;
        for (spec, &table) in files.iter().zip(&tables) {
            let mut offsets: Vec<u32> = Vec::new();
            let mut schemas: Vec<[u32; 4]> = Vec::new();
            let mut instances: Vec<u64> = Vec::new();
            for name in &spec.messages {
                let layout = self.layouts[name].clone();
                let msg = self.schema.message(name).unwrap().clone();
                let start = offsets.len() as u32;
                offsets.extend([layout.has_bits_offset, 8, !0, !0, !0, !0]);
                for f in &msg.fields {
                    offsets.push(layout.fields.get(&f.number).map_or(!0, |fl| fl.offset));
                }
                let has_bits_at = offsets.len() as u32;
                for f in &msg.fields {
                    offsets.push(
                        layout
                            .fields
                            .get(&f.number)
                            .and_then(|fl| fl.has_bit)
                            .unwrap_or(!0),
                    );
                }
                schemas.push([start, has_bits_at, !0, layout.size as u32]);
                let at = self.image.alloc_data(layout.size);
                put_u64(&mut self.image.bytes, at, self.vtables[name]);
                instances.push(MODULE_BASE + at as u64);
            }

            let put_array = |image: &mut Image, bytes: &[u8]| {
                let at = image.alloc_rdata(bytes.len().max(1));
                image.bytes[at..at + bytes.len()].copy_from_slice(bytes);
                at
            };
            let words = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let offsets_at = put_array(&mut self.image, &words(&offsets));
            let schemas_at = put_array(&mut self.image, &words(&schemas.concat()));
            let instances_at = put_array(
                &mut self.image,
                &instances
                    .iter()
                    .flat_map(|p| p.to_le_bytes())
                    .collect::<Vec<u8>>(),
            );
            let mut name_nul = spec.name.clone().into_bytes();
            name_nul.push(0);
            let name_at = put_array(&mut self.image, &name_nul);
            let descriptor = spec.descriptor.clone().unwrap_or_else(|| {
                let mut d = vec![0x0a, spec.name.len() as u8];
                d.extend_from_slice(spec.name.as_bytes());
                d
            });
            let descriptor_at = put_array(&mut self.image, &descriptor);
            let dep_ptrs: Vec<u8> = spec
                .deps
                .iter()
                .flat_map(|d| {
                    let i = files
                        .iter()
                        .position(|f| &f.name == d)
                        .expect("dep emitted");
                    abs(tables[i]).to_le_bytes()
                })
                .collect();
            let deps_at = put_array(&mut self.image, &dep_ptrs);

            put_u32(&mut self.image.bytes, table, 1);
            put_u32(&mut self.image.bytes, table + 4, descriptor.len() as u32);
            put_u64(&mut self.image.bytes, table + 8, abs(descriptor_at));
            put_u64(&mut self.image.bytes, table + 16, abs(name_at));
            put_u64(&mut self.image.bytes, table + 32, abs(deps_at));
            put_u32(&mut self.image.bytes, table + 40, spec.deps.len() as u32);
            put_u32(
                &mut self.image.bytes,
                table + 44,
                spec.messages.len() as u32,
            );
            put_u64(&mut self.image.bytes, table + 48, abs(schemas_at));
            put_u64(&mut self.image.bytes, table + 56, abs(instances_at));
            put_u64(&mut self.image.bytes, table + 64, abs(offsets_at));
        }
        self.commit();
    }

    /// Emit the real serialized descriptors, and the offset tables, of every file that
    /// declares a message this world has a class for, plus the files those import. What a
    /// live client carries, so `client_schema` and `ClientTables::read` work on it.
    pub fn emit_real_tables(&mut self) {
        use prost::Message as _;
        use std::collections::BTreeMap;

        let pool = &*valveprotos::deadlock::DESCRIPTOR_POOL;
        let mut closure = BTreeMap::new();
        let mut todo: Vec<_> = self
            .layouts
            .keys()
            .map(|m| pool.get_message_by_name(m).unwrap().parent_file())
            .collect();
        while let Some(file) = todo.pop() {
            if closure
                .insert(file.name().to_string(), file.clone())
                .is_none()
            {
                todo.extend(file.dependencies());
            }
        }
        let mut specs: Vec<FileTables> = closure
            .values()
            .map(|file| FileTables {
                name: file.name().to_string(),
                descriptor: Some(file.file_descriptor_proto().encode_to_vec()),
                deps: file.dependencies().map(|d| d.name().to_string()).collect(),
                messages: self
                    .layouts
                    .keys()
                    .filter(|m| pool.get_message_by_name(m).unwrap().parent_file() == *file)
                    .cloned()
                    .collect(),
            })
            .collect();
        specs.sort_by_key(|s| std::cmp::Reverse(s.deps.len()));
        self.emit_file_tables(&specs);
    }

    /// Push the local image and heap into the mock address space.
    pub fn commit(&mut self) {
        let mut mem = MockMemory::new(4242);
        mem.write(MODULE_BASE, &self.image.bytes);
        mem.add_module("client.dll", MODULE_BASE, MODULE_SIZE);
        mem.write(HEAP_BASE, &self.heap.bytes[..self.heap.used()]);
        mem.add_region(HEAP_BASE, self.heap.used());
        mem.add_region(MODULE_BASE + DATA_RVA as u64, DATA_END - DATA_RVA);
        self.mem = mem;
    }

    /// Point the first vtable slot of `name` at non-code memory.
    pub fn corrupt_first_slot(&mut self, name: &str) {
        let at = (self.vtables[name] - MODULE_BASE) as usize;
        put_u64(&mut self.image.bytes, at, MODULE_BASE + DATA_RVA as u64);
        self.commit();
    }

    pub fn patch(&mut self, addr: u64, data: &[u8]) {
        self.heap.put(addr, data);
        self.commit();
    }

    pub fn read_u64(&self, addr: u64) -> u64 {
        let at = (addr - HEAP_BASE) as usize;
        u64::from_le_bytes(self.heap.bytes[at..at + 8].try_into().unwrap())
    }

    /// Build the object for `wire` as message `name`. Returns its address.
    pub fn build(&mut self, name: &str, wire: &[u8]) -> u64 {
        let addr = self.build_inner(name, wire);
        self.commit();
        addr
    }

    fn build_inner(&mut self, name: &str, wire: &[u8]) -> u64 {
        let layout = self.layouts[name].clone();
        let msg = self.schema.message(name).unwrap().clone();
        let addr = self.heap.alloc(layout.size, 8);
        let mut obj = vec![0u8; layout.size];
        put_u64(&mut obj, 0, self.vtables[name]);

        let values = parse_wire(wire);
        for f in &msg.fields {
            let (Some(fl), Some(vals)) = (layout.fields.get(&f.number), values.get(&f.number))
            else {
                continue;
            };
            let off = fl.offset as usize;
            if let Some(bit) = fl.has_bit {
                let at = layout.has_bits_offset as usize + (bit / 32) as usize * 4;
                let word =
                    u32::from_le_bytes(obj[at..at + 4].try_into().unwrap()) | 1 << (bit % 32);
                put_u32(&mut obj, at, word);
            }
            self.store_field(&mut obj, off, f, vals);
        }
        self.heap.put(addr, &obj);
        addr
    }

    fn store_field(&mut self, obj: &mut [u8], off: usize, f: &FieldSchema, vals: &[Value<'_>]) {
        match (&f.kind, f.repeated) {
            (FieldKind::Scalar(s), false) => {
                let b = scalar_bytes(*s, vals.last().unwrap());
                obj[off..off + b.len()].copy_from_slice(&b);
            }
            (FieldKind::Scalar(s), true) => {
                let items = scalar_items(*s, vals);
                // The field points at element 0; the arena pointer sits just before it.
                let block = self.heap.alloc(8 + items.len() * s.size(), 8);
                let elements = block + 8;
                for (i, it) in items.iter().enumerate() {
                    self.heap.put(elements + (i * s.size()) as u64, it);
                }
                put_u32(obj, off, items.len() as u32);
                put_u32(obj, off + 4, items.len() as u32);
                put_u64(obj, off + 8, elements);
            }
            (FieldKind::String | FieldKind::Bytes, false) => {
                let Value::Bytes(b) = vals.last().unwrap() else {
                    panic!("string field without bytes")
                };
                let s = self.alloc_std_string(b);
                put_u64(obj, off, s | 1);
            }
            (FieldKind::Message(sub), false) => {
                let Value::Bytes(b) = vals.last().unwrap() else {
                    panic!("message field without bytes")
                };
                let p = self.build_inner(sub, b);
                put_u64(obj, off, p);
            }
            (kind, true) => {
                let ptrs: Vec<u64> = vals
                    .iter()
                    .map(|v| {
                        let Value::Bytes(b) = v else {
                            panic!("repeated ptr element without bytes")
                        };
                        match kind {
                            FieldKind::Message(sub) => self.build_inner(sub, b),
                            _ => self.alloc_std_string(b),
                        }
                    })
                    .collect();
                let rep = self.heap.alloc(8 + ptrs.len() * 8, 8);
                put_u32(
                    &mut self.heap.bytes[(rep - HEAP_BASE) as usize..],
                    0,
                    ptrs.len() as u32,
                );
                for (i, p) in ptrs.iter().enumerate() {
                    self.heap.put(rep + 8 + (i * 8) as u64, &p.to_le_bytes());
                }
                put_u32(obj, off + 8, ptrs.len() as u32);
                put_u32(obj, off + 12, ptrs.len() as u32);
                put_u64(obj, off + 16, rep);
            }
            (FieldKind::Unsupported(_), false) => {}
        }
    }

    /// An MSVC `std::string`: 16-byte SSO buffer or heap pointer, size, capacity.
    fn alloc_std_string(&mut self, data: &[u8]) -> u64 {
        let s = self.heap.alloc(32, 8);
        let mut raw = [0u8; 32];
        if data.len() < 16 {
            raw[..data.len()].copy_from_slice(data);
            put_u64(&mut raw, 24, 15);
        } else {
            let buf = self.heap.alloc(data.len() + 1, 8);
            self.heap.put(buf, data);
            put_u64(&mut raw, 0, buf);
            put_u64(&mut raw, 24, data.len() as u64);
        }
        put_u64(&mut raw, 16, data.len() as u64);
        self.heap.put(s, &raw);
        s
    }

    /// Address of field `number`'s storage inside the object at `obj`.
    pub fn field_addr(&self, message: &str, obj: u64, number: u32) -> u64 {
        obj + u64::from(self.layouts[message].fields[&number].offset)
    }
}

fn compute_layout(msg: &MessageSchema) -> MessageLayout {
    let singular = msg
        .fields
        .iter()
        .filter(|f| !f.repeated && !matches!(f.kind, FieldKind::Unsupported(_)))
        .count();
    let has_bits_offset = 16usize;
    let mut cursor = has_bits_offset + singular.div_ceil(32).max(1) * 4 + 4;

    let mut bit_of = HashMap::new();
    let mut next_bit = 0u32;
    for f in &msg.fields {
        if !f.repeated && !matches!(f.kind, FieldKind::Unsupported(_)) {
            bit_of.insert(f.number, next_bit);
            next_bit += 1;
        }
    }

    let mut order: Vec<&FieldSchema> = msg
        .fields
        .iter()
        .filter(|f| !matches!(f.kind, FieldKind::Unsupported(_)))
        .collect();
    order.sort_by_key(|f| std::cmp::Reverse(f.number));

    let mut fields = BTreeMap::new();
    for f in order {
        let (size, al) = match (&f.kind, f.repeated) {
            (FieldKind::Scalar(_), true) => (16, 8),
            (_, true) => (24, 8),
            (FieldKind::Scalar(s), false) => (s.size(), s.size()),
            _ => (8, 8),
        };
        cursor = align(cursor, al);
        fields.insert(
            f.number,
            FieldLayout {
                offset: cursor as u32,
                has_bit: bit_of.get(&f.number).copied(),
            },
        );
        cursor += size;
    }
    MessageLayout {
        size: align(cursor, 8),
        has_bits_offset: has_bits_offset as u32,
        fields,
    }
}

/// The Statlocker cache, if present on this machine. Override with
/// `DEADLOCK_WALKER_FIXTURE`.
pub fn statlocker_fixture() -> Option<Vec<u8>> {
    use std::io::Read;
    let path = std::env::var("DEADLOCK_WALKER_FIXTURE").ok().or_else(|| {
        let base = std::env::var("LOCALAPPDATA").ok()?;
        Some(format!(
            r"{base}\statlocker-companion\metadata_cache\match_109598353.pb.gz"
        ))
    })?;
    let gz = std::fs::read(path).ok()?;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

/// A small hand-made match: enough variety to touch every field shape the walker reads.
pub fn sample_match() -> valveprotos::deadlock::CMsgMatchMetaDataContents {
    use valveprotos::deadlock::CMsgMatchMetaDataContents as Root;
    use valveprotos::deadlock::c_msg_match_meta_data_contents::{
        Deaths, MatchInfo, Objective, Players, Position,
    };
    let player = |slot: u32| Players {
        account_id: Some(1_000 + slot),
        player_slot: Some(slot),
        team: Some(i32::from(slot % 2 == 1)),
        kills: Some(slot * 3),
        stats_type_stat: vec![0.5, 1.5, slot as f32],
        net_worth: Some(u32::MAX - slot),
        death_details: vec![Deaths {
            game_time_s: Some(60 * slot),
            time_to_kill_s: Some(1.5),
            death_pos: Some(Position {
                x: Some(-12.25),
                y: Some(0.0),
                z: Some(4096.5),
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    Root {
        match_info: Some(MatchInfo {
            duration_s: Some(788),
            match_id: Some(109_598_353),
            players: (0..4).map(player).collect(),
            objectives: vec![
                Objective {
                    legacy_objective_id: Some(-1),
                    destroyed_time_s: Some(300),
                    ..Default::default()
                },
                Objective::default(),
            ],
            low_pri_pool: Some(true),
            custom_user_stats: vec![
                valveprotos::deadlock::c_msg_match_meta_data_contents::CustomUserStatInfo {
                    name: Some("short".into()),
                    id: Some(1),
                },
                valveprotos::deadlock::c_msg_match_meta_data_contents::CustomUserStatInfo {
                    name: Some("a name longer than the sixteen byte buffer".into()),
                    id: Some(2),
                },
            ],
            ..Default::default()
        }),
    }
}
