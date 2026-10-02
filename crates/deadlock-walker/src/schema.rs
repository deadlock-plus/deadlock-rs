//! Field numbers and wire types, taken from the `valveprotos` descriptor pool.
//!
//! The walker never trusts the target for *what* a field is, only for *where* it is. Types
//! come from here, offsets from a [`crate::layout::LayoutSource`].

use std::collections::HashMap;

/// Scalar protobuf types, i.e. everything with a fixed in-memory width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    /// `double`
    Double,
    /// `float`
    Float,
    /// `int32`
    Int32,
    /// `int64`
    Int64,
    /// `uint32`
    UInt32,
    /// `uint64`
    UInt64,
    /// `sint32` (zigzag on the wire)
    SInt32,
    /// `sint64` (zigzag on the wire)
    SInt64,
    /// `fixed32`
    Fixed32,
    /// `fixed64`
    Fixed64,
    /// `sfixed32`
    SFixed32,
    /// `sfixed64`
    SFixed64,
    /// `bool`, one byte in memory
    Bool,
    /// An enum, an `int32` in memory
    Enum,
}

impl Scalar {
    /// Width of one value in the C++ object.
    pub fn size(self) -> usize {
        match self {
            Scalar::Bool => 1,
            Scalar::Float
            | Scalar::Int32
            | Scalar::UInt32
            | Scalar::SInt32
            | Scalar::Fixed32
            | Scalar::SFixed32
            | Scalar::Enum => 4,
            Scalar::Double
            | Scalar::Int64
            | Scalar::UInt64
            | Scalar::SInt64
            | Scalar::Fixed64
            | Scalar::SFixed64 => 8,
        }
    }
}

/// What a field holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// A fixed-width value.
    Scalar(Scalar),
    /// A UTF-8 `string`.
    String,
    /// A `bytes` blob.
    Bytes,
    /// A nested message, by full name.
    Message(String),
    /// Not readable: map, group or oneof member. The reason is kept for the error.
    Unsupported(&'static str),
}

/// One field of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSchema {
    /// Field number on the wire.
    pub number: u32,
    /// Field name, for diagnostics.
    pub name: String,
    /// Element type.
    pub kind: FieldKind,
    /// `repeated`.
    pub repeated: bool,
    /// Packed encoding on the wire (repeated scalars only).
    pub packed: bool,
}

/// One message type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageSchema {
    /// Full protobuf name, e.g. `CMsgMatchMetaDataContents.Players`.
    pub name: String,
    /// Fields in declaration order. Declaration order is also has-bit order.
    pub fields: Vec<FieldSchema>,
}

/// All known messages, by full name.
#[derive(Clone, Debug, Default)]
pub struct Schema {
    messages: HashMap<String, MessageSchema>,
}

impl Schema {
    /// Build a schema from hand-made messages.
    pub fn from_messages(messages: impl IntoIterator<Item = MessageSchema>) -> Self {
        Schema {
            messages: messages.into_iter().map(|m| (m.name.clone(), m)).collect(),
        }
    }

    /// Every message in the pinned `valveprotos` descriptor pool.
    pub fn from_valveprotos() -> Self {
        Self::from_pool(&valveprotos::deadlock::DESCRIPTOR_POOL)
    }

    /// Every message in `pool`.
    pub fn from_pool(pool: &prost_reflect::DescriptorPool) -> Self {
        let messages = pool
            .all_messages()
            .filter(|m| !m.is_map_entry())
            .map(|m| MessageSchema {
                name: m.full_name().to_string(),
                fields: declaration_order(&m)
                    .into_iter()
                    .map(|f| {
                        let proto = f.field_descriptor_proto();
                        let kind = if f.is_map() {
                            FieldKind::Unsupported("map")
                        } else if f.is_group() {
                            FieldKind::Unsupported("group")
                        } else if f.containing_oneof().is_some() {
                            FieldKind::Unsupported("oneof")
                        } else {
                            kind_of(
                                proto.r#type.unwrap_or_default(),
                                proto.type_name.as_deref().unwrap_or_default(),
                            )
                        };
                        FieldSchema {
                            number: f.number(),
                            name: f.name().to_string(),
                            kind,
                            repeated: f.is_list(),
                            packed: f.is_packed(),
                        }
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        Schema::from_messages(messages)
    }

    /// Look a message up by full name.
    pub fn message(&self, name: &str) -> Option<&MessageSchema> {
        self.messages.get(name)
    }

    /// Every message name.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.messages.keys().map(String::as_str)
    }
}

/// `fields()` yields in field-number order; the compiled tables follow `.proto` order, which
/// the raw descriptor keeps.
fn declaration_order(
    message: &prost_reflect::MessageDescriptor,
) -> Vec<prost_reflect::FieldDescriptor> {
    message
        .descriptor_proto()
        .field
        .iter()
        .filter_map(|f| message.get_field(f.number() as u32))
        .collect()
}

/// Map `FieldDescriptorProto.Type` numbers.
fn kind_of(ty: i32, type_name: &str) -> FieldKind {
    use Scalar::*;
    match ty {
        1 => FieldKind::Scalar(Double),
        2 => FieldKind::Scalar(Float),
        3 => FieldKind::Scalar(Int64),
        4 => FieldKind::Scalar(UInt64),
        5 => FieldKind::Scalar(Int32),
        6 => FieldKind::Scalar(Fixed64),
        7 => FieldKind::Scalar(Fixed32),
        8 => FieldKind::Scalar(Bool),
        9 => FieldKind::String,
        11 => FieldKind::Message(type_name.trim_start_matches('.').to_string()),
        12 => FieldKind::Bytes,
        13 => FieldKind::Scalar(UInt32),
        14 => FieldKind::Scalar(Enum),
        15 => FieldKind::Scalar(SFixed32),
        16 => FieldKind::Scalar(SFixed64),
        17 => FieldKind::Scalar(SInt32),
        18 => FieldKind::Scalar(SInt64),
        _ => FieldKind::Unsupported("group or unknown type"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiled offset tables list fields in `.proto` declaration order, which is not
    /// number order: `CMsgMatchMetaDataContents.Players` declares field 48 before field 27.
    #[test]
    fn fields_are_in_declaration_order_not_number_order() {
        let schema = Schema::from_valveprotos();
        let players = schema.message("CMsgMatchMetaDataContents.Players").unwrap();
        let position = |number| {
            players
                .fields
                .iter()
                .position(|f| f.number == number)
                .unwrap()
        };
        assert!(position(48) < position(27));
    }

    #[test]
    fn the_pinned_pool_knows_the_match_metadata_tree() {
        let schema = Schema::from_valveprotos();
        let root = schema.message("CMsgMatchMetaDataContents").unwrap();
        let info = root.fields.iter().find(|f| f.number == 2).unwrap();
        assert_eq!(
            info.kind,
            FieldKind::Message("CMsgMatchMetaDataContents.MatchInfo".into())
        );
        let info = schema
            .message("CMsgMatchMetaDataContents.MatchInfo")
            .unwrap();
        assert!(info.fields.iter().any(|f| f.repeated));
    }
}
