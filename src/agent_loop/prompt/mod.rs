//! System prompt sources, templates, and assembly.

mod builder;
mod sources;

/// Upper bound on one prompt file written from the WebUI.
pub(crate) const MAX_PROMPT_FILE_BYTES: usize = 256 * 1024;

pub(crate) use builder::build_system_prompt_with_config;
pub(crate) use sources::{
    PromptFileKind, PromptFileLayer, PromptFileScope, PromptFileWriteError, SoulAgentsLoader,
    prompt_content_hash,
};
