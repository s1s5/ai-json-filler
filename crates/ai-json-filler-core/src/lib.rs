pub mod error;
pub mod feedback;
pub mod give_up;
pub mod prompt;
pub mod schema;
pub mod session;
pub mod tool;
pub mod typed_tool;
pub mod validation;

pub use error::FillError;
pub use feedback::FeedbackOptions;
pub use give_up::{DEFAULT_GIVE_UP_TOOL_NAME, GiveUp};
pub use schema::FillSchema;
pub use session::{FillOutcome, FillSession, SessionResult};
pub use tool::{Tool, ToolOutput, ToolSet, ToolSpec};
pub use typed_tool::{TypedTool, typed_tool};
pub use validation::ValidationIssue;

/// Default name of the tool that performs partial saves.
pub const DEFAULT_SAVE_TOOL_NAME: &str = "save_result";

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
