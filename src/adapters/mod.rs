//! Adapters for turning various types into [`Body`]s.
//!
//! [`Body`]: crate::Body

mod async_read;
mod read;

pub use async_read::AsyncReadBody;
pub use read::ReadBody;
