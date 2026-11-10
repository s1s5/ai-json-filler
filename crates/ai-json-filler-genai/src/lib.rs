//! `genai` integration for [`ai_json_filler_core`].
//!
//! [`fill`] drives a provider-neutral [`FillSession`](core::FillSession) against any
//! model that `genai` can talk to, translating the session's tool specs into
//! `genai` tools and feeding tool results back into the conversation.

pub mod backend;
pub mod builder;
pub mod convert;

pub use ai_json_filler_core as core;
pub use backend::{ChatBackend, GenaiBackend};
pub use builder::{GenaiFillBuilder, fill, fill_target, fill_with_backend};
pub use convert::to_genai_tool;
