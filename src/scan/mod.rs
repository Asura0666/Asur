pub mod allowlist;
pub mod entropy;
pub mod git;
pub mod mask;
pub mod rules;
pub mod walker;

pub use allowlist::Allowlist;
#[allow(unused_imports)]
pub use entropy::shannon_entropy;
#[allow(unused_imports)]
pub use git::is_git_repo;
pub use git::{
    GitRepoStatus, check_git_repo_status, install_pre_commit_hook, scan_git_history,
    scan_staged_changes,
};
#[allow(unused_imports)]
pub use mask::mask_secret;
pub use rules::{Finding, SecretScanner};
#[allow(unused_imports)]
pub use walker::WorkingTreeReport;
pub use walker::{SkippedFile, scan_working_tree_with_progress};
