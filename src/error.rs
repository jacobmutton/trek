use serde::Serialize;

#[allow(dead_code)] // some variants only used once later commands land
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NoWorkspace,
    Locked,
    DirtyWorktree,
    DetachedHead,
    MidOperation,
    UnpushedCommits,
    BranchNotFound,
    BranchExists,
    WorktreeExists,
    DriftDetected,
    AlreadyStaged,
    NotStaged,
    TicketNotFound,
    SuffixNotFound,
    SuffixExists,
    RepoNotInWorkspace,
    InvalidTicketId,
    PartialFailure,
    NotImplemented,
    Internal,
}

impl ErrorCode {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::NoWorkspace
            | Self::InvalidTicketId
            | Self::RepoNotInWorkspace
            | Self::TicketNotFound
            | Self::SuffixNotFound => 1,
            Self::Locked
            | Self::DirtyWorktree
            | Self::DetachedHead
            | Self::MidOperation
            | Self::UnpushedCommits
            | Self::DriftDetected
            | Self::AlreadyStaged
            | Self::NotStaged
            | Self::BranchExists
            | Self::WorktreeExists
            | Self::SuffixExists
            | Self::BranchNotFound => 2,
            Self::PartialFailure => 3,
            Self::NotImplemented | Self::Internal => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_errors_exit_1() {
        for c in [
            ErrorCode::NoWorkspace,
            ErrorCode::InvalidTicketId,
            ErrorCode::RepoNotInWorkspace,
            ErrorCode::TicketNotFound,
            ErrorCode::SuffixNotFound,
        ] {
            assert_eq!(c.exit_code(), 1, "{c:?}");
        }
    }

    #[test]
    fn precondition_errors_exit_2() {
        for c in [
            ErrorCode::Locked,
            ErrorCode::DirtyWorktree,
            ErrorCode::DetachedHead,
            ErrorCode::MidOperation,
            ErrorCode::UnpushedCommits,
            ErrorCode::DriftDetected,
            ErrorCode::AlreadyStaged,
            ErrorCode::NotStaged,
            ErrorCode::BranchExists,
            ErrorCode::WorktreeExists,
            ErrorCode::SuffixExists,
            ErrorCode::BranchNotFound,
        ] {
            assert_eq!(c.exit_code(), 2, "{c:?}");
        }
    }

    #[test]
    fn partial_failure_exits_3() {
        assert_eq!(ErrorCode::PartialFailure.exit_code(), 3);
    }

    #[test]
    fn internal_errors_exit_4() {
        assert_eq!(ErrorCode::NotImplemented.exit_code(), 4);
        assert_eq!(ErrorCode::Internal.exit_code(), 4);
    }

    #[test]
    fn serializes_as_screaming_snake() {
        let s = serde_json::to_string(&ErrorCode::DirtyWorktree).unwrap();
        assert_eq!(s, "\"DIRTY_WORKTREE\"");
        let s = serde_json::to_string(&ErrorCode::AlreadyStaged).unwrap();
        assert_eq!(s, "\"ALREADY_STAGED\"");
    }
}
