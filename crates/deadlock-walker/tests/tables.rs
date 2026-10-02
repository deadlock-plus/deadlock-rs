//! Layouts derived from the compiled tables must equal what the "compiler" chose, and must
//! drive the walker to the same bytes.

mod support;

use deadlock_memory::mem::MemoryReader;
use deadlock_walker::{
    ClientTables, Error, LayoutSource, PeImage, SearchConfig, VtableResolver, Walker,
    client_schema, find_instances,
};
use prost::Message;
use support::*;
use valveprotos::deadlock::CMsgMatchMetaDataContents as Root;

const ROOT: &str = "CMsgMatchMetaDataContents";

fn two_files(w: &World) -> Vec<(&'static str, Vec<String>)> {
    let mut names: Vec<String> = w.layouts.keys().cloned().collect();
    names.sort();
    let second = names.split_off(names.len() / 2);
    vec![("first.proto", names), ("second.proto", second)]
}

fn image(w: &World) -> PeImage {
    let module = w.mem.module("client.dll").unwrap();
    PeImage::read(&w.mem, &module).unwrap()
}

#[test]
fn derived_layouts_equal_the_ones_the_compiler_chose() {
    let mut w = World::new(ROOT);
    let files = two_files(&w);
    w.emit_tables(&files);

    let tables = ClientTables::read(&image(&w), &w.schema).unwrap();
    assert!(tables.skipped().is_empty(), "{:?}", tables.skipped());
    assert_eq!(tables.len(), w.layouts.len());
    for (name, expected) in &w.layouts {
        assert_eq!(tables.layout(name), Some(expected), "{name}");
    }
}

#[test]
fn derived_layouts_drive_the_walker_to_the_original_message() {
    let original = sample_match();
    let mut w = World::new(ROOT);
    let files = two_files(&w);
    w.emit_tables(&files);
    w.build(ROOT, &original.encode_to_vec());

    let module = w.mem.module("client.dll").unwrap();
    let image = image(&w);
    let tables = ClientTables::read(&image, &w.schema).unwrap();
    let vtable = VtableResolver::new(&image).vtable(ROOT).unwrap();
    let config = SearchConfig::default().excluding_module(&module);
    let found = find_instances(&w.mem, vtable, &config).unwrap();
    assert_eq!(found.len(), 1);

    let wire = Walker::new(&w.mem, &w.schema, &tables)
        .serialize_checked(ROOT, found[0], vtable)
        .unwrap();
    assert_eq!(Root::decode(&wire[..]).unwrap(), original);
}

#[test]
fn a_message_whose_field_count_disagrees_with_the_pool_is_skipped_not_guessed() {
    let mut w = World::new(ROOT);
    let files = two_files(&w);
    w.emit_tables(&files);

    let victim = "CMsgMatchMetaDataContents.Players";
    let mut stale = w.schema.message(victim).unwrap().clone();
    stale.fields.pop();
    let others = w
        .schema
        .names()
        .filter(|n| *n != victim)
        .map(|n| w.schema.message(n).unwrap().clone())
        .chain([stale]);
    let schema = deadlock_walker::schema::Schema::from_messages(others);

    let tables = ClientTables::read(&image(&w), &schema).unwrap();
    assert!(tables.layout(victim).is_none());
    assert_eq!(tables.skipped().len(), 1);
    assert_eq!(tables.skipped()[0].message, victim);
    assert!(tables.layout(ROOT).is_some());
}

/// The pinned pool and the client can disagree on a field's shape while agreeing on how many
/// fields there are. Offsets are only trusted when they fit the shapes the pool claims.
fn skipped_after(mutate: impl FnOnce(&mut deadlock_walker::schema::FieldSchema)) -> ClientTables {
    let mut w = World::new(ROOT);
    let files = two_files(&w);
    w.emit_tables(&files);

    let victim = "CMsgMatchMetaDataContents.Players";
    let mut changed = w.schema.message(victim).unwrap().clone();
    mutate(
        changed
            .fields
            .iter_mut()
            .find(|f| f.name == "kills")
            .unwrap(),
    );
    let others = w
        .schema
        .names()
        .filter(|n| *n != victim)
        .map(|n| w.schema.message(n).unwrap().clone())
        .chain([changed]);
    let schema = deadlock_walker::schema::Schema::from_messages(others);
    let tables = ClientTables::read(&image(&w), &schema).unwrap();
    assert!(tables.layout(victim).is_none(), "layout was derived");
    assert_eq!(tables.skipped().len(), 1);
    tables
}

#[test]
fn a_field_that_gained_repeated_in_the_pool_is_skipped() {
    skipped_after(|f| f.repeated = true);
}

#[test]
fn a_field_that_widened_in_the_pool_is_skipped() {
    use deadlock_walker::schema::{FieldKind, Scalar};
    skipped_after(|f| f.kind = FieldKind::Scalar(Scalar::UInt64));
}

/// Every file `ROOT` imports, transitively, as real serialized descriptors, emitted
/// dependents first so a reader that assumes dependency order fails. `edit` may rewrite a
/// file's descriptor before it is embedded.
fn emit_real_files(
    w: &mut World,
    mut edit: impl FnMut(&mut prost_reflect::prost_types::FileDescriptorProto),
) {
    use std::collections::BTreeMap;

    let pool = &*valveprotos::deadlock::DESCRIPTOR_POOL;
    let mut closure = BTreeMap::new();
    let mut todo = vec![pool.get_message_by_name(ROOT).unwrap().parent_file()];
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
        .map(|file| {
            let mut proto = file.file_descriptor_proto().clone();
            edit(&mut proto);
            FileTables {
                name: file.name().to_string(),
                descriptor: Some(proto.encode_to_vec()),
                deps: file.dependencies().map(|d| d.name().to_string()).collect(),
                messages: w
                    .layouts
                    .keys()
                    .filter(|m| pool.get_message_by_name(m).unwrap().parent_file() == *file)
                    .cloned()
                    .collect(),
            }
        })
        .collect();
    specs.sort_by_key(|s| std::cmp::Reverse(s.deps.len()));
    w.emit_file_tables(&specs);
}

#[test]
fn the_client_schema_equals_the_pool_it_was_compiled_from() {
    let mut w = World::new(ROOT);
    emit_real_files(&mut w, |_| {});

    let client = client_schema(&image(&w)).unwrap();
    let pinned = deadlock_walker::schema::Schema::from_valveprotos();
    for name in w.layouts.keys() {
        assert_eq!(client.message(name), pinned.message(name), "{name}");
    }
}

#[test]
fn the_client_schema_follows_the_embedded_descriptor_not_the_pinned_one() {
    use deadlock_walker::schema::{FieldKind, Scalar};
    use prost_reflect::prost_types::field_descriptor_proto::Type;

    let mut w = World::new(ROOT);
    emit_real_files(&mut w, |file| {
        fn visit(msg: &mut prost_reflect::prost_types::DescriptorProto, path: &str) {
            let path = format!("{path}.{}", msg.name());
            if path.ends_with("CMsgMatchMetaDataContents.Players") {
                let kills = msg.field.iter_mut().find(|f| f.name() == "kills").unwrap();
                kills.set_type(Type::Bool);
            }
            for nested in &mut msg.nested_type {
                visit(nested, &path);
            }
        }
        for msg in &mut file.message_type {
            visit(msg, "");
        }
    });

    let client = client_schema(&image(&w)).unwrap();
    let players = client.message("CMsgMatchMetaDataContents.Players").unwrap();
    let kills = players.fields.iter().find(|f| f.name == "kills").unwrap();
    assert_eq!(kills.kind, FieldKind::Scalar(Scalar::Bool));
}

#[test]
fn an_image_without_descriptor_tables_has_no_client_schema() {
    let w = World::new(ROOT);
    let err = client_schema(&image(&w)).unwrap_err();
    assert!(matches!(err, Error::NoTables), "{err}");
}

#[test]
fn an_image_without_descriptor_tables_is_an_error() {
    let w = World::new(ROOT);
    let err = ClientTables::read(&image(&w), &w.schema).unwrap_err();
    assert!(matches!(err, Error::NoTables), "{err}");
}
