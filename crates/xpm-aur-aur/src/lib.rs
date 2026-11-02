//! AUR protocol and source adapters.
//!
//! This crate knows about AUR RPC, AUR git repositories and `.SRCINFO`; it does
//! not know about pacman, xpm or the local filesystem layout outside a checkout.

pub mod error;
pub mod git;
pub mod rpc;
pub mod srcinfo;

pub use error::{AurError, AurResult};
pub use git::AurGit;
pub use rpc::{AurPackage, AurRpc, SearchBy};
pub use srcinfo::{SrcInfo, SrcPackage, parse_srcinfo, read_srcinfo};
