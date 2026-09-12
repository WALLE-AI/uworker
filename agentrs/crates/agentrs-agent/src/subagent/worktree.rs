use std::path::{Path, PathBuf};

use tokio::process::Command as TokioCommand;

use agentrs_types::subagent::SubAgentId;

pub(crate) struct Worktree {
    repository: PathBuf,
    path: PathBuf,
    active: bool,
}

pub(crate) enum Cleanup {
    Removed,
    Preserved(PathBuf),
}

impl Worktree {
    pub(crate) async fn create(cwd: &Path, id: &SubAgentId) -> Result<Self, String> {
        let repository = git_stdout(cwd, &["rev-parse", "--show-toplevel"]).await?;
        let repository = PathBuf::from(repository.trim());
        let path = std::env::temp_dir().join("agentrs-worktrees").join(id.as_str());
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| format!("unable to create worktree directory: {error}"))?;
        }
        let path_arg = path.to_string_lossy().into_owned();
        git_stdout(&repository, &["worktree", "add", "--detach", &path_arg, "HEAD"]).await?;
        Ok(Self {
            repository,
            path,
            active: true,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn cleanup_if_clean(mut self) -> Result<Cleanup, String> {
        let status = git_stdout(&self.path, &["status", "--porcelain"]).await?;
        if !status.trim().is_empty() {
            self.active = false;
            return Ok(Cleanup::Preserved(self.path.clone()));
        }
        let path_arg = self.path.to_string_lossy().into_owned();
        git_stdout(&self.repository, &["worktree", "remove", "--force", &path_arg]).await?;
        self.active = false;
        Ok(Cleanup::Removed)
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if !self.active || !self.path.exists() {
            return;
        }
        let Ok(status) = git_stdout_sync(&self.path, &["status", "--porcelain"]) else {
            return;
        };
        if !status.trim().is_empty() {
            tracing::warn!(
                target: "agentrs_agent",
                path = %self.path.display(),
                "preserving dirty isolated worktree after early sub-agent exit"
            );
            return;
        }
        let path_arg = self.path.to_string_lossy().into_owned();
        if let Err(error) = git_stdout_sync(&self.repository, &["worktree", "remove", "--force", &path_arg]) {
            tracing::warn!(target: "agentrs_agent", %error, "unable to clean isolated worktree after early exit");
        }
    }
}

async fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = TokioCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("unable to run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn git_stdout_sync(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|error| format!("unable to run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
#[path = "worktree_test.rs"]
mod worktree_test;
