//! `roub-server`: the authoritative tier. Stateless; all long-lived state lives in a `Store`.
//!
//! The binary in `main.rs` wires `RoubService` to a store and a listener. The library exists so
//! integration tests can run the same service in process.

mod convert;
pub mod memory;
pub mod service;
pub mod store;

pub use memory::MemoryStore;
pub use service::RoubService;
pub use store::{Store, StoreError};
