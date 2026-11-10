use ai_json_filler_core::ToolSpec;
use genai::chat::{Tool, ToolName};

/// Convert a provider-neutral [`ToolSpec`] into a `genai` [`Tool`].
pub fn to_genai_tool(spec: &ToolSpec) -> Tool {
    Tool::new(ToolName::Custom(spec.name.clone()))
        .with_description(spec.description.clone())
        .with_schema(spec.parameters.clone())
}
