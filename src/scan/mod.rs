pub mod allowlist;
pub mod entropy;
pub mod git;
pub mod mask;
pub mod rules;
pub mod walker;

pub use allowlist::Allowlist;
#[allow(unused_imports)]
pub use entropy::shannon_entropy;
pub use git::{install_pre_commit_hook, is_git_repo, scan_git_history, scan_staged_changes};
#[allow(unused_imports)]
pub use mask::mask_secret;
pub use rules::{Finding, SecretScanner};
#[allow(unused_imports)]
pub use walker::WorkingTreeReport;
pub use walker::{SkippedFile, scan_working_tree};
