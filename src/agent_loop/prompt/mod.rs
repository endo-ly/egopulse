//! System prompt sources, templates, and assembly.

mod builder;
mod sources;

pub(crate) use builder::build_system_prompt_with_config;
pub(crate) use sources::SoulAgentsLoader;
