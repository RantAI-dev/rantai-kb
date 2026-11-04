//! Object storage adapter for S3-compatible stores (S3, MinIO, RustFS).

#![allow(clippy::module_name_repetitions)]

mod s3;

pub use s3::{S3BlobStore, S3Settings, sign_v4};
