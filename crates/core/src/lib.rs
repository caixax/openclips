//! Platform independent domain logic for OpenClips.
//!
//! Nothing in this crate may depend on an operating system API or on a media
//! framework. Capture backends and the UI both depend on this crate and talk
//! to each other only through the types defined here.

pub mod capture;
pub mod clip;
pub mod config;
pub mod error;
pub mod library;
pub mod logging;
pub mod media;
pub mod replay;

pub use config::Config;
pub use error::{CoreError, Result};

pub const APP_NAME: &str = "OpenClips";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
