// Copyright 2025 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::io;
use std::num::NonZeroU32;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

use crate::git::GitPushOptions;
use crate::git::GitPushStats;
use crate::git::GitSubprocessOptions;
use crate::git::NegativeRefSpec;
use crate::git::RefSpec;
use crate::git::RefToPush;
use crate::git_backend::GitBackend;
use crate::merge::Diff;
use crate::ref_name::GitRefNameBuf;
use crate::ref_name::RefNameBuf;
use crate::ref_name::RemoteName;

/// Error originating from a Git remote or worktree operation
#[derive(Error, Debug)]
pub enum GitSubprocessError {
    #[error("Could not find repository at '{0}'")]
    NoSuchRepository(String),
    #[error("Failed to report progress")]
    Callback(#[source] io::Error),
    #[error("Git operation failed: {0}")]
    External(String),
    #[error(transparent)]
    Transport(grit_lib::remote::RemoteError),
    #[error("Failed to open the Git repository")]
    OpenRepository(#[source] grit_lib::error::Error),
    #[error("Failed to update Git worktrees")]
    Worktree(#[source] grit_lib::error::Error),
}

/// Context for running Git remote and worktree operations
pub(crate) struct GitSubprocessContext {
    git_dir: PathBuf,
    options: GitSubprocessOptions,
}

impl GitSubprocessContext {
    pub(crate) fn new(git_dir: impl Into<PathBuf>, options: GitSubprocessOptions) -> Self {
        Self {
            git_dir: git_dir.into(),
            options,
        }
    }

    pub(crate) fn from_git_backend(
        git_backend: &GitBackend,
        options: GitSubprocessOptions,
    ) -> Self {
        Self::new(git_backend.git_repo_path(), options)
    }

    fn open_repo(&self) -> Result<grit_lib::repo::Repository, GitSubprocessError> {
        // Layer the configured variables over the process environment, as
        // they would be passed to a `git` subprocess.
        let cwd = std::env::current_dir().unwrap_or_default();
        let vars = std::env::vars_os().chain(
            self.options
                .environment
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        let environment = grit_lib::prelude::Environment::from_vars(vars, cwd);
        crate::git_backend::open_grit_repo_in_environment(&self.git_dir, environment)
            .map_err(GitSubprocessError::OpenRepository)
    }

    /// Perform a git fetch
    ///
    /// [`GitFetchStatus::NoRemoteRef`] is returned if ref doesn't exist. Note
    /// that `git` only returns one failed ref at a time.
    pub(crate) fn fetch(
        &self,
        remote_name: &RemoteName,
        refspecs: &[RefSpec],
        negative_refspecs: &[NegativeRefSpec],
        callback: &mut dyn GitSubprocessCallback,
        depth: Option<NonZeroU32>,
    ) -> Result<GitFetchStatus, GitSubprocessError> {
        let repo = self.open_repo()?;
        crate::git_transport::fetch(
            &repo,
            remote_name,
            refspecs,
            negative_refspecs,
            callback,
            depth,
        )
    }

    /// Looks up the branch the remote's `HEAD` points to.
    pub(crate) fn default_branch(
        &self,
        remote_name: &RemoteName,
    ) -> Result<Option<RefNameBuf>, GitSubprocessError> {
        let repo = self.open_repo()?;
        crate::git_transport::default_branch(&repo, remote_name)
    }

    /// Push references to the remote
    ///
    /// All pushes are forced, with the expected remote values checked like
    /// `--force-with-lease` to perform a test&set operation on the remote
    /// repository.
    pub(crate) fn push(
        &self,
        remote_name: &RemoteName,
        references: &[RefToPush],
        callback: &mut dyn GitSubprocessCallback,
        options: &GitPushOptions,
    ) -> Result<GitPushStats, GitSubprocessError> {
        let repo = self.open_repo()?;
        crate::git_transport::push(&repo, remote_name, references, callback, options)
    }

    /// Create a Git worktree at `destination` on the unborn branch
    /// `branch_name`.
    ///
    /// Nothing is checked out and the repo doesn't need any commits. The
    /// branch is left unborn, so the caller can move HEAD off it before anyone
    /// sees the worktree.
    pub(crate) fn add_orphan_worktree(
        &self,
        destination: &Path,
        branch_name: &str,
    ) -> Result<(), GitSubprocessError> {
        let repo = self.open_repo()?;
        crate::git_transport::add_orphan_worktree(&repo, destination, branch_name)
    }

    /// Fix up the paths Git recorded for the worktree at `worktree_path`,
    /// after the worktree or the repository has been moved.
    pub(crate) fn repair_worktree(&self, worktree_path: &Path) -> Result<(), GitSubprocessError> {
        let repo = self.open_repo()?;
        crate::git_transport::repair_worktree(&repo, worktree_path)
    }

    /// Remove the bookkeeping of worktrees whose directories are gone.
    pub(crate) fn prune_worktrees(&self) -> Result<(), GitSubprocessError> {
        let repo = self.open_repo()?;
        crate::git_transport::prune_worktrees(&repo)
    }
}

/// Status of a fetch operation.
#[derive(Clone, Debug)]
pub enum GitFetchStatus {
    /// Successfully fetched refs. There may be refs that couldn't be updated.
    Updates(GitRefUpdates),
    /// Fully-qualified ref that failed to fetch.
    ///
    /// Note that only one error is returned at a time.
    NoRemoteRef(String),
}

/// Local changes made by a fetch.
#[derive(Clone, Debug, Default)]
pub struct GitRefUpdates {
    /// Git ref `(name, (old_oid, new_oid))`s that are successfully updated.
    ///
    /// `old_oid`/`new_oid` may be null or point to non-commit objects such as
    /// tags.
    #[expect(dead_code)] // unused as of now
    pub updated: Vec<(GitRefNameBuf, Diff<grit_lib::objects::ObjectId>)>,
    /// Git ref `(name, (old_oid, new_oid)`s that are rejected or failed to
    /// update.
    pub rejected: Vec<(GitRefNameBuf, Diff<grit_lib::objects::ObjectId>)>,
}

/// Handles progress and messages from Git remote operations.
pub trait GitSubprocessCallback {
    /// Whether to request progress information.
    fn needs_progress(&self) -> bool;

    /// Progress of local and remote operations.
    fn progress(&mut self, progress: &GitProgress) -> io::Result<()>;

    /// Single-line message that doesn't look like remote sideband or error.
    ///
    /// This may include authentication request from credential helpers.
    fn local_sideband(
        &mut self,
        message: &[u8],
        term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()>;

    /// Single-line sideband message received from remote.
    fn remote_sideband(
        &mut self,
        message: &[u8],
        term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()>;
}

/// Newline character that terminates sideband message line.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum GitSidebandLineTerminator {
    /// CR to remain on the same line.
    Cr = b'\r',
    /// LF to move to the next line.
    Lf = b'\n',
}

impl GitSidebandLineTerminator {
    /// Returns byte representation.
    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

/// Progress of a Git remote operation.
#[derive(Clone, Debug, Default)]
pub struct GitProgress {
    /// `(frac, total)` of "Resolving deltas".
    pub deltas: (u64, u64),
    /// `(frac, total)` of "Receiving objects".
    pub objects: (u64, u64),
    /// `(frac, total)` of remote "Counting objects".
    pub counted_objects: (u64, u64),
    /// `(frac, total)` of remote "Compressing objects".
    pub compressed_objects: (u64, u64),
}

// TODO: maybe let callers print each field separately and remove overall()?
impl GitProgress {
    /// Overall progress normalized to 0 to 1 range.
    pub fn overall(&self) -> f32 {
        if self.total() != 0 {
            self.fraction() as f32 / self.total() as f32
        } else {
            0.0
        }
    }

    fn fraction(&self) -> u64 {
        self.objects.0 + self.deltas.0 + self.counted_objects.0 + self.compressed_objects.0
    }

    fn total(&self) -> u64 {
        self.objects.1 + self.deltas.1 + self.counted_objects.1 + self.compressed_objects.1
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_initial_overall_progress_is_zero() {
        assert_eq!(GitProgress::default().overall(), 0.0);
    }
}
