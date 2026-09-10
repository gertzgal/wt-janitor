pub mod activity;
pub mod cleanup;
pub mod cli;
pub mod config;
pub mod deps;
pub mod discovery;
pub mod doctor;
pub mod error;
pub mod git;
pub mod models;
pub mod proc;
pub mod progress;
pub mod report;
pub mod sizes;
pub mod state;

pub use config::{default_config_path, default_state_path, load_config, select_repos};
pub use discovery::recommend;
pub use error::{
    Error, Result, EXIT_DEPENDENCY, EXIT_OK, EXIT_OPERATIONAL, EXIT_SAFETY, EXIT_USAGE,
};
