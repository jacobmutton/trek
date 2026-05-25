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
