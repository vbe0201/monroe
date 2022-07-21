//! TODO

#![deny(missing_docs, rust_2018_idioms, rustdoc::broken_intra_doc_links)]
#![feature(generic_associated_types, never_type, type_alias_impl_trait)]

#[doc(no_inline)]
pub use monroe_inbox::{Id, RecvError, SendError, TryRecvError, TrySendError};

mod actor;
pub use self::actor::*;

mod address;
pub use self::address::*;

mod context;
pub use self::context::*;

mod supervisor;
pub use self::supervisor::*;
