//! Composition root: typed config, axum router, auth, handlers, error mapping.

#![allow(clippy::module_name_repetitions)]

mod auth;
mod config;
mod dto;
mod error;
mod routes;
mod state;

pub use auth::{authenticate_request, AuthState};
pub use config::Config;
pub use error::ApiError;
pub use state::{build_app, AppState};