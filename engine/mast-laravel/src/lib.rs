//! Laravel-side domain: for now the lossless `.env` model (plan §6, M5).
//! Sail detection lives in `mast-project`; artisan and the service catalog
//! arrive with M7.

pub mod env;
pub mod env_write;
pub mod ports;
pub mod processes;
pub mod url;
pub mod validate;

pub use env::{EnvEntry, EnvError, EnvFile, EnvItem, Quoting};
pub use env_write::{EnvWriteError, edit_env_file};
pub use ports::{is_host_port_key, next_free_port};
pub use url::app_url;
pub use validate::{Finding, Severity, is_secret_key, validate};
