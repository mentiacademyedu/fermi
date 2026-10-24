//! fermi-core: routing, account pool and provider adapters.
//!
//! Every front end (CLI, TUI, the future web dashboard) drives the same
//! [`Engine`] and consumes the same [`Event`] stream.

pub mod approvals;
pub mod auth;
pub mod billing;
pub mod code;
pub mod config;
pub mod deck;
pub mod engine;
pub mod events;
pub mod hub;
pub mod mcp;
pub mod memory;
pub mod models;
pub mod policy;
pub mod providers;
pub mod redact;
pub mod router;
pub mod sandbox;
pub mod sessions;
pub mod state;
pub mod team;
pub mod trace;
pub mod util;
pub mod verify;

pub use config::{Account, Auth, Config, Mode, Profile, Provider};
pub use engine::{Engine, Outcome, Task};
pub use events::{Event, Tx};
