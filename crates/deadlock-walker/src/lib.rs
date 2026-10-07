//! Reads live C++ protobuf objects out of a running Deadlock client.
//!
//! The pipeline, each step usable alone:
//!
//! 1. [`pe::PeImage`] copies `client.dll` out of the target.
//! 2. [`rtti::VtableResolver`] turns a protobuf message name into the vtable address of its
//!    generated C++ class, via MSVC RTTI.
//! 3. [`search::find_instances`] finds heap objects whose first qword is that vtable.
//! 4. [`tables::client_schema`] decodes the client's own embedded descriptors and
//!    [`tables::ClientTables`] reads its compiled offset tables, giving every message's
//!    field layout with nothing guessed.
//! 5. [`walk::Walker`] reads each object by that layout and emits protobuf wire
//!    bytes, which decode with `valveprotos` and `prost`.
//!
//! Read-only: everything goes through [`deadlock_memory::mem::MemoryReader`].

#![forbid(unsafe_code)]

pub mod error;
pub mod ext;
pub mod gc;
pub mod layout;
pub mod pe;
pub mod queue_flag;
pub mod rtti;
pub mod schema;
pub mod search;
pub mod tables;
pub mod walk;

pub use error::{Error, Result};
pub use gc::{GcSession, Kind, SweepReport};
pub use layout::{FieldLayout, LayoutSource, MessageLayout};
pub use pe::PeImage;
pub use queue_flag::{QueueFlag, QueueRequest};
pub use rtti::{VtableResolver, mangled_class_name};
pub use schema::Schema;
pub use search::{SearchConfig, Sweep, find_in, find_instances, find_many};
pub use tables::{ClientTables, Skipped, client_schema};
pub use walk::{Abi, Limits, Walker};
