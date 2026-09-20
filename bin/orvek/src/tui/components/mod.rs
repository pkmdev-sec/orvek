//! Stateful UI components and their event boundary.

mod actions;
mod activity;
mod activity_mark;
mod animation;
mod app;
mod brand;
mod choice;
mod composer;
mod context_diagnostics;
mod dialog;
mod effort;
mod file_finder;
mod keybindings;
mod layout;
mod memory;
mod model_selector;
mod node;
mod queue;
mod recent_prompt_picker;
mod review_confirmation;
mod root;
mod selection;
mod session_picker;
mod skill_picker;
mod startup;
mod subagent_tree_layout;
mod subagents;
#[cfg(test)]
mod testing;
mod theme_selector;
mod toast;
mod transcript;
mod typography;
mod waved_text;

pub(crate) use app::{AppEffect, AppEvent, AppNode};
pub(crate) use node::{ComponentUpdate, RenderRequest};
pub(crate) use queue::{QueueId, QueuedInput};
pub(crate) use root::{DraftReset, RootEffect, RootNode, SessionListKind};
pub(crate) use startup::StartupScreen;
pub(crate) use transcript::image::initialize as initialize_image_renderer;
