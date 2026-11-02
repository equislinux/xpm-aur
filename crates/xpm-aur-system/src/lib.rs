//! Host adapters for xpm-aur.
//!
//! Nothing in this crate is used by the pure resolver except through the ports
//! defined in `xpm-aur-core`.

pub mod catalog;
pub mod pacman_db;
pub mod process;
pub mod state;
pub mod xpm;

pub use catalog::SystemCatalog;
pub use pacman_db::{DbError, PacmanDatabase};
pub use process::SystemRunner;
pub use state::{AurRecord, State, StateError, StateStore};
pub use xpm::{XpmClient, XpmError};
