//! protojson schema tables built from the compiled descriptor set.
//!
//! connect-go v1.18.1 decodes JSON with `protojson` and `DiscardUnknown: true`.
//! The vendored `connectrpc::protojson` walker needs the message graph to
//! reproduce Go's accept/reject decisions and error texts, so this module
//! projects `OUT_DIR/envd_descriptor.bin` (emitted by `build.rs`) into its
//! schema types and registers it once per process.

use buffa_descriptor::{DescriptorPool, ScalarType, SingularKind};
use connectrpc::protojson::{EnumSchema, FieldKind, FieldSchema, MessageSchema, SchemaSet};
use std::sync::OnceLock;

/// Install the schema set used by Connect JSON decoding. Idempotent.
pub(crate) fn ensure_registered() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        if let Some(schema) = build() {
            connectrpc::protojson::register(schema);
        }
    });
}

fn build() -> Option<&'static SchemaSet> {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/envd_descriptor.bin"));
    let pool = DescriptorPool::decode(bytes).ok()?;

    let messages: Vec<MessageSchema> = pool
        .messages()
        .iter()
        .map(|message| {
            let oneofs: Vec<&'static str> = message
                .oneofs()
                .iter()
                .map(|oneof| intern(&format!("{}.{}", message.full_name(), oneof.name())))
                .collect();
            let fields: Vec<FieldSchema> = message
                .fields()
                .iter()
                .map(|field| {
                    let (kind, message_type, enum_type, repeated, map_value, map_key) =
                        match field.kind() {
                            buffa_descriptor::FieldKind::Singular(value) => {
                                let (kind, message_type, enum_type) = singular(&pool, value);
                                (
                                    kind,
                                    message_type,
                                    enum_type,
                                    false,
                                    None,
                                    FieldKind::String,
                                )
                            }
                            buffa_descriptor::FieldKind::List(value) => {
                                let (kind, message_type, enum_type) = singular(&pool, value);
                                (kind, message_type, enum_type, true, None, FieldKind::String)
                            }
                            buffa_descriptor::FieldKind::Map { key, value } => {
                                let (kind, message_type, enum_type) = singular(&pool, value);
                                (
                                    kind,
                                    message_type,
                                    enum_type,
                                    false,
                                    Some(kind),
                                    scalar(key),
                                )
                            }
                        };
                    let oneof = field.oneof_index().and_then(|index| {
                        let oneof = message.oneofs().get(usize::from(index))?;
                        (!oneof.is_synthetic()).then_some(usize::from(index))
                    });
                    FieldSchema {
                        name: intern(field.name()),
                        json_name: intern(field.json_name()),
                        kind,
                        repeated,
                        map_value,
                        map_key,
                        message: message_type,
                        enum_name: enum_type,
                        oneof,
                    }
                })
                .collect();
            MessageSchema {
                full_name: intern(message.full_name()),
                fields: leak_slice(fields),
                oneofs: leak_slice(oneofs),
            }
        })
        .collect();

    let enums: Vec<EnumSchema> = pool
        .enums()
        .iter()
        .map(|enumeration| EnumSchema {
            full_name: intern(enumeration.full_name()),
            values: leak_slice(
                enumeration
                    .values()
                    .iter()
                    .map(|value| (intern(value.name()), value.number()))
                    .collect(),
            ),
        })
        .collect();

    Some(Box::leak(Box::new(SchemaSet {
        messages: leak_slice(messages),
        enums: leak_slice(enums),
    })))
}

fn singular(
    pool: &DescriptorPool,
    kind: SingularKind,
) -> (FieldKind, Option<&'static str>, Option<&'static str>) {
    match kind {
        SingularKind::Scalar(scalar_type) => (scalar(scalar_type), None, None),
        SingularKind::Enum(index) => (
            FieldKind::Enum,
            None,
            Some(intern(pool.enumeration(index).full_name())),
        ),
        SingularKind::Message(index) => (
            FieldKind::Message,
            Some(intern(pool.message(index).full_name())),
            None,
        ),
    }
}

fn scalar(scalar_type: ScalarType) -> FieldKind {
    match scalar_type {
        ScalarType::Double => FieldKind::Double,
        ScalarType::Float => FieldKind::Float,
        ScalarType::Int64 | ScalarType::Sint64 | ScalarType::Sfixed64 => FieldKind::Int64,
        ScalarType::Uint64 | ScalarType::Fixed64 => FieldKind::Uint64,
        ScalarType::Int32 | ScalarType::Sint32 | ScalarType::Sfixed32 => FieldKind::Int32,
        ScalarType::Uint32 | ScalarType::Fixed32 => FieldKind::Uint32,
        ScalarType::Bool => FieldKind::Bool,
        ScalarType::String => FieldKind::String,
        ScalarType::Bytes => FieldKind::Bytes,
    }
}

fn intern(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn leak_slice<T>(values: Vec<T>) -> &'static [T] {
    Box::leak(values.into_boxed_slice())
}
