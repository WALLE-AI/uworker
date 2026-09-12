use tracing::warn;

use agentrs_types::team::TeamError;
use agentrs_types::tool::ToolResult;

pub(super) fn failure(error: TeamError) -> ToolResult {
    warn!(target: "agentrs_tools", kind = error_kind(&error), "team operation rejected");
    ToolResult {
        content: error.to_string(),
        is_error: true,
    }
}

fn error_kind(error: &TeamError) -> &'static str {
    match error {
        TeamError::InvalidName { .. } => "invalid_name",
        TeamError::AlreadyLeading { .. } => "already_leading",
        TeamError::NoCurrentTeam => "no_current_team",
        TeamError::DuplicateName { .. } => "duplicate_name",
        TeamError::UnknownRecipient { .. } => "unknown_recipient",
        TeamError::RecipientGone { .. } => "recipient_gone",
        TeamError::InboxFull { .. } => "inbox_full",
        TeamError::MemberLimit { .. } => "member_limit",
        TeamError::ActiveMembers { .. } => "active_members",
        TeamError::Storage { .. } => "storage",
        TeamError::Runtime { .. } => "runtime",
    }
}
