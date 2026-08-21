//! Agent Loop policy and model/tool iteration state.

use std::sync::Arc;

use crate::llm::Message;

/// Maximum number of model/tool iterations allowed for one activation.
pub(crate) const MAX_TOOL_ITERATIONS: usize = 50;

/// The iteration at which the model is warned that the hard loop limit is near.
pub(crate) const FINAL_RESPONSE_WARNING_ITERATION: usize = MAX_TOOL_ITERATIONS - 2;

/// Runtime guard appended to the iteration-48 request to warn the model that
/// the tool loop is nearing its hard limit and begin final-response preparation.
pub(crate) const FINAL_RESPONSE_WARNING_GUARD: &str = "[runtime_guard]: The tool loop is near its hard limit. Two model iterations remain after this one. Do not start broad new work; prepare the best concise answer for the user now and state any uncertainty. If this is a Pulse activation and you have used tools, summarize the result instead of returning PULSE_OK.";

/// Runtime guard appended to requests in the final-response window to
/// prioritize a concise final response over starting new broad tool work.
pub(crate) const FINAL_RESPONSE_GUARD: &str = "[runtime_guard]: The tool loop is at its final response window. Provide the best concise answer to the user now. Do not start broad new work; state what was completed and any uncertainty. If this is a Pulse activation and you have used tools, summarize the result instead of returning PULSE_OK.";

/// Adds the iteration-specific runtime guard to a request-only copy of `messages`.
///
/// The original messages are not modified.
pub(crate) fn messages_for_iteration(
    messages: &Arc<Vec<Message>>,
    iteration: usize,
) -> Arc<Vec<Message>> {
    let guard = match iteration {
        FINAL_RESPONSE_WARNING_ITERATION => Some(FINAL_RESPONSE_WARNING_GUARD),
        iteration if iteration > FINAL_RESPONSE_WARNING_ITERATION => Some(FINAL_RESPONSE_GUARD),
        _ => None,
    };
    let Some(guard) = guard else {
        return Arc::clone(messages);
    };

    let mut request_messages = (**messages).clone();
    request_messages.push(Message::text("user", guard));
    Arc::new(request_messages)
}
