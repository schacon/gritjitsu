// Copyright 2020 The Jujutsu Authors
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

#![expect(missing_docs)]

use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fmt::Error;
use std::fmt::Formatter;
use std::fs;
use std::io;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Command;
use std::process::ExitStatus;
use std::str::Utf8Error;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::SystemTime;

use async_trait::async_trait;
use futures::AsyncRead;
use futures::AsyncReadExt as _;
use futures::StreamExt as _;
use futures::io::Cursor;
use futures::stream::BoxStream;
use grit_lib::diff::DiffEntry;
use grit_lib::diff::DiffStatus;
use grit_lib::objects::HashAlgo;
use grit_lib::objects::ObjectId as GritObjectId;
use grit_lib::objects::ObjectKind;
use grit_lib::objects::TreeEntry as GritTreeEntry;
use grit_lib::objects::TreeRefIter;
use grit_lib::objects::serialize_tree;
use grit_lib::objects::tree_entry_cmp;
use grit_lib::refs::store::Expected;
use grit_lib::refs::store::RawRef;
use grit_lib::refs::store::RefUpdate;
use grit_lib::refs::store::RefUpdateFlags;
use itertools::Itertools as _;
use once_cell::sync::OnceCell as OnceLock;
use pollster::FutureExt as _;
use prost::Message as _;
use smallvec::SmallVec;
use thiserror::Error;

use crate::backend::Backend;
use crate::backend::BackendError;
use crate::backend::BackendInitError;
use crate::backend::BackendLoadError;
use crate::backend::BackendResult;
use crate::backend::ChangeId;
use crate::backend::Commit;
use crate::backend::CommitId;
use crate::backend::CopyHistory;
use crate::backend::CopyId;
use crate::backend::CopyRecord;
use crate::backend::FileId;
use crate::backend::MillisSinceEpoch;
use crate::backend::RelatedCopy;
use crate::backend::SecureSig;
use crate::backend::Signature;
use crate::backend::SigningFn;
use crate::backend::SymlinkId;
use crate::backend::Timestamp;
use crate::backend::Tree;
use crate::backend::TreeId;
use crate::backend::TreeValue;
use crate::backend::make_root_commit;
use crate::config::ConfigGetError;
use crate::file_util;
use crate::file_util::BadPathEncoding;
use crate::file_util::IoResultExt as _;
use crate::file_util::PathError;
use crate::git::GitSettings;
use crate::lock::FileLock;
use crate::merge::Merge;
use crate::merge::MergeBuilder;
use crate::object_id::ObjectId;
use crate::repo_path::RepoPath;
use crate::repo_path::RepoPathBuf;
use crate::repo_path::RepoPathComponentBuf;
use crate::settings::UserSettings;
use crate::stacked_table::MutableTable;
use crate::stacked_table::ReadonlyTable;
use crate::stacked_table::TableSegment as _;
use crate::stacked_table::TableStore;
use crate::stacked_table::TableStoreError;

const CHANGE_ID_LENGTH: usize = 16;
/// Ref namespace used only for preventing GC.
const NO_GC_REF_NAMESPACE: &str = "refs/jj/keep/";

pub const JJ_CONFLICT_README_FILE_NAME: &str = "JJ-CONFLICT-README";

pub const JJ_TREES_COMMIT_HEADER: &str = "jj:trees";
pub const JJ_CONFLICT_LABELS_COMMIT_HEADER: &str = "jj:conflict-labels";
pub const CHANGE_ID_COMMIT_HEADER: &str = "change-id";

#[derive(Debug, Error)]
pub enum GitBackendInitError {
    #[error("Failed to initialize git repository")]
    InitRepository(#[source] gix::Error),
    #[error("Failed to open git repository")]
    OpenRepository(#[source] gix::Error),
    #[error("Failed to open git repository")]
    OpenGritRepository(#[source] grit_lib::error::Error),
    #[error("Failed to encode git repository path")]
    EncodeRepositoryPath(#[source] BadPathEncoding),
    #[error(transparent)]
    Config(ConfigGetError),
    #[error(transparent)]
    Path(PathError),
}

impl From<Box<GitBackendInitError>> for BackendInitError {
    fn from(err: Box<GitBackendInitError>) -> Self {
        Self(err)
    }
}

#[derive(Debug, Error)]
pub enum GitBackendLoadError {
    #[error("Failed to open git repository")]
    OpenRepository(#[source] gix::Error),
    #[error("Failed to open git repository")]
    OpenGritRepository(#[source] grit_lib::error::Error),
    #[error("Failed to decode git repository path")]
    DecodeRepositoryPath(#[source] BadPathEncoding),
    #[error(transparent)]
    Config(ConfigGetError),
    #[error(transparent)]
    Path(PathError),
}

impl From<Box<GitBackendLoadError>> for BackendLoadError {
    fn from(err: Box<GitBackendLoadError>) -> Self {
        Self(err)
    }
}

/// `GitBackend`-specific error that may occur after the backend is loaded.
#[derive(Debug, Error)]
pub enum GitBackendError {
    #[error("Failed to read non-git metadata")]
    ReadMetadata(#[source] TableStoreError),
    #[error("Failed to write non-git metadata")]
    WriteMetadata(#[source] TableStoreError),
}

impl From<GitBackendError> for BackendError {
    fn from(err: GitBackendError) -> Self {
        Self::Other(err.into())
    }
}

#[derive(Debug, Error)]
pub enum GitRepoAtWorkdirError {
    #[error("No Git repository found at {path}")]
    NotFound {
        path: PathBuf,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Unrelated Git repository found at {path}")]
    Unrelated { path: PathBuf },
    #[error("Failed to open Git repository")]
    Other(#[source] Box<dyn std::error::Error + Send + Sync>),
}

#[derive(Debug, Error)]
pub enum GitGcError {
    #[error("Failed to run git gc command")]
    GcCommand(#[source] std::io::Error),
    #[error("git gc command exited with an error: {0}")]
    GcCommandErrorStatus(ExitStatus),
}

pub struct GitBackend {
    // While gix::Repository can be created from gix::ThreadSafeRepository, it's
    // cheaper to cache the thread-local instance behind a mutex than creating
    // one for each backend method call. Our GitBackend is most likely to be
    // used in a single-threaded context.
    base_repo: gix::ThreadSafeRepository,
    repo: Mutex<gix::Repository>,
    grit_repo: grit_lib::repo::Repository,
    root_commit_id: CommitId,
    root_change_id: ChangeId,
    empty_tree_id: TreeId,
    shallow_root_ids: OnceLock<Vec<CommitId>>,
    extra_metadata_store: TableStore,
    cached_extra_metadata: Mutex<Option<Arc<ReadonlyTable>>>,
    git_executable: PathBuf,
    write_change_id_header: bool,
    /// `Name <email>` recorded in reflog entries.
    reflog_identity: String,
}

impl GitBackend {
    pub const NAME: &str = "git";

    fn new(
        base_repo: gix::ThreadSafeRepository,
        grit_repo: grit_lib::repo::Repository,
        extra_metadata_store: TableStore,
        git_settings: GitSettings,
        reflog_identity: String,
    ) -> Self {
        let repo = base_repo.to_thread_local();
        let hash_algo = grit_repo.odb().hash_algo();
        let root_commit_id = CommitId::from_bytes(GritObjectId::null(hash_algo).as_bytes());
        let root_change_id = ChangeId::from_bytes(&[0; CHANGE_ID_LENGTH]);
        let empty_tree_id = TreeId::from_bytes(empty_tree_oid(hash_algo).as_bytes());
        Self {
            base_repo,
            repo: Mutex::new(repo),
            grit_repo,
            root_commit_id,
            root_change_id,
            empty_tree_id,
            shallow_root_ids: OnceLock::new(),
            extra_metadata_store,
            cached_extra_metadata: Mutex::new(None),
            git_executable: git_settings.executable_path,
            write_change_id_header: git_settings.write_change_id_header,
            reflog_identity,
        }
    }

    pub fn init_internal(
        settings: &UserSettings,
        store_path: &Path,
        object_hash: gix::hash::Kind,
    ) -> Result<Self, Box<GitBackendInitError>> {
        let git_repo_path = Path::new("git");
        let git_repo = gix::ThreadSafeRepository::init_opts(
            store_path.join(git_repo_path),
            gix::create::Kind::Bare,
            gix::create::Options {
                object_hash: Some(object_hash),
                ..Default::default()
            },
            gix_open_opts_from_settings(settings),
        )
        .map_err(GitBackendInitError::InitRepository)?;
        let git_settings =
            GitSettings::from_settings(settings).map_err(GitBackendInitError::Config)?;
        Self::init_with_repo(
            store_path,
            git_repo_path,
            git_repo,
            git_settings,
            reflog_identity(settings),
        )
    }

    /// Initializes backend by creating a new Git repo at the specified
    /// workspace path. The workspace directory must exist.
    pub fn init_colocated(
        settings: &UserSettings,
        store_path: &Path,
        workspace_root: &Path,
        object_hash: gix::hash::Kind,
    ) -> Result<Self, Box<GitBackendInitError>> {
        let canonical_workspace_root = {
            let path = store_path.join(workspace_root);
            dunce::canonicalize(&path)
                .context(&path)
                .map_err(GitBackendInitError::Path)?
        };
        let git_repo = gix::ThreadSafeRepository::init_opts(
            canonical_workspace_root,
            gix::create::Kind::WithWorktree,
            gix::create::Options {
                object_hash: Some(object_hash),
                ..Default::default()
            },
            gix_open_opts_from_settings(settings),
        )
        .map_err(GitBackendInitError::InitRepository)?;
        let git_repo_path = workspace_root.join(".git");
        let git_settings =
            GitSettings::from_settings(settings).map_err(GitBackendInitError::Config)?;
        Self::init_with_repo(
            store_path,
            &git_repo_path,
            git_repo,
            git_settings,
            reflog_identity(settings),
        )
    }

    /// Initializes backend with an existing Git repo at the specified path.
    pub fn init_external(
        settings: &UserSettings,
        store_path: &Path,
        git_repo_path: &Path,
    ) -> Result<Self, Box<GitBackendInitError>> {
        let canonical_git_repo_path = {
            let path = store_path.join(git_repo_path);
            canonicalize_git_repo_path(&path)
                .context(&path)
                .map_err(GitBackendInitError::Path)?
        };
        let git_repo = gix::ThreadSafeRepository::open_opts(
            canonical_git_repo_path,
            gix_open_opts_from_settings(settings),
        )
        .map_err(GitBackendInitError::OpenRepository)?;
        let git_settings =
            GitSettings::from_settings(settings).map_err(GitBackendInitError::Config)?;
        Self::init_with_repo(
            store_path,
            git_repo_path,
            git_repo,
            git_settings,
            reflog_identity(settings),
        )
    }

    fn init_with_repo(
        store_path: &Path,
        git_repo_path: &Path,
        repo: gix::ThreadSafeRepository,
        git_settings: GitSettings,
        reflog_identity: String,
    ) -> Result<Self, Box<GitBackendInitError>> {
        let extra_path = store_path.join("extra");
        fs::create_dir(&extra_path)
            .context(&extra_path)
            .map_err(GitBackendInitError::Path)?;
        let target_path = store_path.join("git_target");
        let git_repo_path = if cfg!(windows) && git_repo_path.is_relative() {
            // When a repository is created in Windows, format the path with *forward
            // slashes* and not backwards slashes. This makes it possible to use the same
            // repository under Windows Subsystem for Linux.
            //
            // This only works for relative paths. If the path is absolute, there's not much
            // we can do, and it simply won't work inside and outside WSL at the same time.
            file_util::slash_path(git_repo_path)
        } else {
            git_repo_path.into()
        };
        let git_repo_path_bytes = file_util::path_to_bytes(&git_repo_path)
            .map_err(GitBackendInitError::EncodeRepositoryPath)?;
        fs::write(&target_path, git_repo_path_bytes)
            .context(&target_path)
            .map_err(GitBackendInitError::Path)?;
        let grit_repo =
            open_grit_repo(repo.path()).map_err(GitBackendInitError::OpenGritRepository)?;
        let extra_metadata_store = TableStore::init(extra_path, grit_repo.odb().hash_algo().len());
        Ok(Self::new(
            repo,
            grit_repo,
            extra_metadata_store,
            git_settings,
            reflog_identity,
        ))
    }

    pub fn load(
        settings: &UserSettings,
        store_path: &Path,
    ) -> Result<Self, Box<GitBackendLoadError>> {
        let git_repo_path = {
            let target_path = store_path.join("git_target");
            let git_repo_path_bytes = fs::read(&target_path)
                .context(&target_path)
                .map_err(GitBackendLoadError::Path)?;
            let git_repo_path = file_util::path_from_bytes(&git_repo_path_bytes)
                .map_err(GitBackendLoadError::DecodeRepositoryPath)?;
            let git_repo_path = store_path.join(git_repo_path);
            canonicalize_git_repo_path(&git_repo_path)
                .context(&git_repo_path)
                .map_err(GitBackendLoadError::Path)?
        };
        let repo = gix::ThreadSafeRepository::open_opts(
            git_repo_path,
            gix_open_opts_from_settings(settings),
        )
        .map_err(GitBackendLoadError::OpenRepository)?;
        let grit_repo =
            open_grit_repo(repo.path()).map_err(GitBackendLoadError::OpenGritRepository)?;
        let extra_metadata_store =
            TableStore::load(store_path.join("extra"), grit_repo.odb().hash_algo().len());
        let git_settings =
            GitSettings::from_settings(settings).map_err(GitBackendLoadError::Config)?;
        Ok(Self::new(
            repo,
            grit_repo,
            extra_metadata_store,
            git_settings,
            reflog_identity(settings),
        ))
    }

    fn lock_git_repo(&self) -> MutexGuard<'_, gix::Repository> {
        self.repo.lock().unwrap()
    }

    /// Returns a new thread-local handle for the underlying Git repository.
    ///
    /// Use [`Self::open_git_repo_at_workdir()`] for worktree operations.
    pub fn git_repo(&self) -> gix::Repository {
        self.base_repo.to_thread_local()
    }

    /// Returns the grit-lib handle for the underlying Git repository.
    pub fn grit_repo(&self) -> &grit_lib::repo::Repository {
        &self.grit_repo
    }

    /// Opens a new grit-lib handle for the underlying Git repository, picking
    /// up configuration changes made since this backend was loaded.
    pub fn reopen_grit_repo(&self) -> Result<grit_lib::repo::Repository, grit_lib::error::Error> {
        open_grit_repo(self.grit_repo.git_dir())
    }

    /// `Name <email>` to record in reflog entries.
    pub(crate) fn reflog_identity(&self) -> &str {
        &self.reflog_identity
    }

    /// Opens the repository at the given workspace path with grit-lib.
    ///
    /// For a linked Git worktree, the returned repository's HEAD is the
    /// worktree's HEAD.
    pub fn open_grit_repo_at_workdir(
        &self,
        path: &Path,
    ) -> Result<grit_lib::repo::Repository, GitRepoAtWorkdirError> {
        let open_repo = &self.grit_repo;
        if let Some(workdir) = open_repo.work_tree()
            && (workdir == path || dunce::canonicalize(path).is_ok_and(|path| workdir == path))
        {
            return open_grit_repo_with_work_tree(open_repo.git_dir(), Some(workdir))
                .map_err(|err| GitRepoAtWorkdirError::Other(err.into()));
        }

        let dot_git = path.join(".git");
        let git_dir = grit_lib::repo::resolve_dot_git(&dot_git).map_err(|source| {
            if dot_git.exists() {
                GitRepoAtWorkdirError::Other(source.into())
            } else {
                let path = path.to_owned();
                GitRepoAtWorkdirError::NotFound {
                    path,
                    source: source.into(),
                }
            }
        })?;
        let work_repo = open_grit_repo_with_work_tree(&git_dir, Some(path))
            .map_err(|err| GitRepoAtWorkdirError::Other(err.into()))?;
        let canonicalize = |path: &Path| {
            dunce::canonicalize(path).map_err(|err| GitRepoAtWorkdirError::Other(err.into()))
        };
        if open_repo.common_git_dir() == work_repo.common_git_dir()
            || canonicalize(open_repo.common_git_dir())?
                == canonicalize(work_repo.common_git_dir())?
        {
            Ok(work_repo)
        } else {
            let path = path.to_owned();
            Err(GitRepoAtWorkdirError::Unrelated { path })
        }
    }

    /// Reopens the repository at the given workspace path. Returns a new
    /// thread-local handle.
    pub fn open_git_repo_at_workdir(
        &self,
        path: &Path,
    ) -> Result<gix::Repository, GitRepoAtWorkdirError> {
        // Try the open repository first.
        let open_repo = self.git_repo();
        if let Some(workdir) = open_repo.workdir()
            && (workdir == path || dunce::canonicalize(path).is_ok_and(|path| workdir == path))
        {
            return Ok(open_repo);
        }

        // The input path doesn't include ".git".
        let opts = open_repo.open_options().clone().open_path_as_is(false);
        let work_repo = gix::ThreadSafeRepository::open_opts(path, opts)
            .map_err(|source| {
                if source.is_not_found() {
                    let path = path.to_owned();
                    GitRepoAtWorkdirError::NotFound {
                        path,
                        source: source.into(),
                    }
                } else {
                    GitRepoAtWorkdirError::Other(source.into())
                }
            })?
            .to_thread_local();
        let canonicalize = |path: &Path| {
            dunce::canonicalize(path).map_err(|err| GitRepoAtWorkdirError::Other(err.into()))
        };
        if open_repo.common_dir() == work_repo.common_dir()
            || canonicalize(open_repo.common_dir())? == canonicalize(work_repo.common_dir())?
        {
            // The last (path, work_repo) can be cached if needed.
            Ok(work_repo)
        } else {
            let path = path.to_owned();
            Err(GitRepoAtWorkdirError::Unrelated { path })
        }
    }

    /// Path to the `.git` directory or the repository itself if it's bare.
    pub fn git_repo_path(&self) -> &Path {
        self.base_repo.path()
    }

    fn shallow_root_ids(&self) -> BackendResult<&[CommitId]> {
        // It's expensive to stat the shallow file on every read_object() call.
        // Refreshing shallow roots is also bad for consistency reasons.
        self.shallow_root_ids
            .get_or_try_init(|| read_shallow_root_ids(self.grit_repo.common_git_dir()))
            .map(AsRef::as_ref)
    }

    fn cached_extra_metadata_table(&self) -> BackendResult<Arc<ReadonlyTable>> {
        let mut locked_head = self.cached_extra_metadata.lock().unwrap();
        match locked_head.as_ref() {
            Some(head) => Ok(head.clone()),
            None => {
                let table = self
                    .extra_metadata_store
                    .get_head()
                    .map_err(GitBackendError::ReadMetadata)?;
                *locked_head = Some(table.clone());
                Ok(table)
            }
        }
    }

    fn read_extra_metadata_table_locked(&self) -> BackendResult<(Arc<ReadonlyTable>, FileLock)> {
        let table = self
            .extra_metadata_store
            .get_head_locked()
            .map_err(GitBackendError::ReadMetadata)?;
        Ok(table)
    }

    fn save_extra_metadata_table(
        &self,
        mut_table: MutableTable,
        _table_lock: &FileLock,
    ) -> BackendResult<()> {
        let table = self
            .extra_metadata_store
            .save_table(mut_table)
            .map_err(GitBackendError::WriteMetadata)?;
        // Since the parent table was the head, saved table are likely to be new head.
        // If it's not, cache will be reloaded when entry can't be found.
        *self.cached_extra_metadata.lock().unwrap() = Some(table);
        Ok(())
    }

    /// Imports the given commits and ancestors from the backing Git repo.
    ///
    /// The `head_ids` may contain commits that have already been imported, but
    /// the caller should filter them out to eliminate redundant I/O processing.
    #[tracing::instrument(skip(self, head_ids))]
    pub fn import_head_commits<'a>(
        &self,
        head_ids: impl IntoIterator<Item = &'a CommitId>,
    ) -> BackendResult<()> {
        let head_ids: HashSet<&CommitId> = head_ids
            .into_iter()
            .filter(|&id| *id != self.root_commit_id)
            .collect();
        if head_ids.is_empty() {
            return Ok(());
        }

        // Create no-gc ref even if known to the extras table. Concurrent GC
        // process might have deleted the no-gc ref.
        edit_references(
            &self.grit_repo,
            head_ids.iter().copied().map(to_no_gc_ref_update),
        )?;

        // These commits are imported from Git. Make our change ids persist (otherwise
        // future write_commit() could reassign new change id.)
        tracing::debug!(
            heads_count = head_ids.len(),
            "import extra metadata entries"
        );
        let (table, table_lock) = self.read_extra_metadata_table_locked()?;
        let mut mut_table = table.start_mutation();
        import_extra_metadata_entries_from_heads(
            &self.grit_repo,
            &mut mut_table,
            &table_lock,
            &head_ids,
            self.shallow_root_ids()?,
        )?;
        self.save_extra_metadata_table(mut_table, &table_lock)
    }

    fn read_file_sync(&self, id: &FileId) -> BackendResult<Vec<u8>> {
        read_object_of_kind(&self.grit_repo, id, ObjectKind::Blob)
    }

    fn read_tree_id_for_commit(&self, id: &CommitId) -> BackendResult<GritObjectId> {
        let tree = self.read_commit(id).block_on()?.root_tree;
        // TODO(kfm): probably want to do something here if it is a merge
        let tree_id = tree.first();
        validate_git_object_id(&self.grit_repo, tree_id)
    }

    fn write_blob(&self, bytes: &[u8], object_type: &'static str) -> BackendResult<GritObjectId> {
        write_object(&self.grit_repo, ObjectKind::Blob, bytes, object_type)
    }
}

/// Canonicalizes the given `path` except for the last `".git"` component.
///
/// The last path component matters when opening a Git repo without `core.bare`
/// config. This config is usually set, but the "repo" tool will set up such
/// repositories and symlinks. Opening such repo with fully-canonicalized path
/// would turn a colocated Git repo into a bare repo.
pub fn canonicalize_git_repo_path(path: &Path) -> io::Result<PathBuf> {
    if path.ends_with(".git") {
        let workdir = path.parent().unwrap();
        dunce::canonicalize(workdir).map(|dir| dir.join(".git"))
    } else {
        dunce::canonicalize(path)
    }
}

fn gix_open_opts_from_settings(settings: &UserSettings) -> gix::open::Options {
    let user_name = settings.user_name();
    let user_email = settings.user_email();
    gix::open::Options::default()
        .config_overrides([
            // Committer has to be configured to record reflog. Author isn't
            // needed, but let's copy the same values.
            format!("author.name={user_name}"),
            format!("author.email={user_email}"),
            format!("committer.name={user_name}"),
            format!("committer.email={user_email}"),
        ])
        // The git_target path should point the repository, not the working directory.
        .open_path_as_is(true)
        // Gitoxide recommends this when correctness is preferred
        .strict_config(true)
}

/// Opens the Git repository at `git_dir` with grit-lib.
fn open_grit_repo(git_dir: &Path) -> Result<grit_lib::repo::Repository, grit_lib::error::Error> {
    open_grit_repo_with_work_tree(git_dir, None)
}

fn open_grit_repo_with_work_tree(
    git_dir: &Path,
    work_tree: Option<&Path>,
) -> Result<grit_lib::repo::Repository, grit_lib::error::Error> {
    let mut options = grit_lib::prelude::RepositoryOptions::with_environment(
        grit_lib::prelude::Environment::capture_process(),
    );
    if let Some(work_tree) = work_tree {
        options = options.with_work_tree(work_tree);
    }
    grit_lib::repo::Repository::open(git_dir, &options)
}

/// Formats the user as `Name <email>` for reflog entries.
fn reflog_identity(settings: &UserSettings) -> String {
    // Git rejects reflog identities with angle brackets or newlines, and
    // empty names. Empty values are written like in commits.
    let clean = |value: &str| {
        let value = value.replace(['<', '>', '\n'], "");
        if value.is_empty() {
            EMPTY_STRING_PLACEHOLDER.to_owned()
        } else {
            value
        }
    };
    format!(
        "{} <{}>",
        clean(settings.user_name()),
        clean(settings.user_email())
    )
}

/// Reads the shallow roots recorded in `$GIT_DIR/shallow`.
fn read_shallow_root_ids(common_dir: &Path) -> BackendResult<Vec<CommitId>> {
    let path = common_dir.join("shallow");
    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(BackendError::Other(err.into())),
    };
    content
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|hex| {
            CommitId::try_from_hex(hex).ok_or_else(|| {
                let line = String::from_utf8_lossy(hex);
                BackendError::Other(format!("Invalid shallow entry: {line}").into())
            })
        })
        .collect()
}

/// Returns the empty tree id for the given hash algorithm.
fn empty_tree_oid(hash_algo: HashAlgo) -> GritObjectId {
    grit_lib::objects::hash::hash_object(hash_algo, ObjectKind::Tree, b"").unwrap()
}

fn read_object_of_kind(
    repo: &grit_lib::repo::Repository,
    id: &impl ObjectId,
    kind: ObjectKind,
) -> BackendResult<Vec<u8>> {
    let oid = validate_git_object_id(repo, id)?;
    let object = repo.odb().read(&oid).map_err(|err| match err {
        grit_lib::error::Error::ObjectNotFound(_) => not_found_err(id),
        err => to_read_object_err(err, id),
    })?;
    if object.kind != kind {
        return Err(to_read_object_err(
            format!("expected {kind} object, found {}", object.kind),
            id,
        ));
    }
    Ok(object.data)
}

fn write_object(
    repo: &grit_lib::repo::Repository,
    kind: ObjectKind,
    data: &[u8],
    object_type: &'static str,
) -> BackendResult<GritObjectId> {
    repo.odb()
        .write(kind, data)
        .map_err(|err| BackendError::WriteObject {
            object_type,
            source: Box::new(err),
        })
}

/// Commit object fields, borrowed from the raw object data.
///
/// The decoding of header values follows Git: continuation lines of a
/// multi-line header lose their leading space.
struct ParsedCommit<'a> {
    data: &'a [u8],
    tree: GritObjectId,
    parents: Vec<GritObjectId>,
    author: &'a [u8],
    committer: &'a [u8],
    /// Name, decoded value, and byte range of the whole header in `data`.
    extra_headers: Vec<(&'a [u8], Cow<'a, [u8]>, Range<usize>)>,
    message: &'a [u8],
}

impl<'a> ParsedCommit<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, String> {
        let mut tree = None;
        let mut parents = Vec::new();
        let mut author = None;
        let mut committer = None;
        let mut extra_headers = Vec::new();
        let mut pos = 0;
        loop {
            if pos >= data.len() {
                return Err("missing blank line before message".to_owned());
            }
            if data[pos] == b'\n' {
                pos += 1;
                break;
            }
            let start = pos;
            let line_end = find_line_end(data, pos);
            let line = &data[start..line_end];
            let (name, first_value) = match line.iter().position(|b| *b == b' ') {
                Some(i) => (&line[..i], &line[i + 1..]),
                None => (line, &b""[..]),
            };
            pos = (line_end + 1).min(data.len());
            let mut continuation_lines = Vec::new();
            while data.get(pos) == Some(&b' ') {
                let end = find_line_end(data, pos);
                continuation_lines.push(&data[pos + 1..end]);
                pos = (end + 1).min(data.len());
            }
            match name {
                b"tree" if tree.is_none() => tree = Some(parse_header_oid(first_value)?),
                b"parent" if author.is_none() => parents.push(parse_header_oid(first_value)?),
                b"author" if author.is_none() => author = Some(first_value),
                b"committer" if committer.is_none() => committer = Some(first_value),
                _ => {
                    let value = if continuation_lines.is_empty() {
                        Cow::Borrowed(first_value)
                    } else {
                        let mut value = first_value.to_vec();
                        for line in continuation_lines {
                            value.push(b'\n');
                            value.extend_from_slice(line);
                        }
                        value.push(b'\n');
                        Cow::Owned(value)
                    };
                    extra_headers.push((name, value, start..pos));
                }
            }
        }
        Ok(Self {
            data,
            tree: tree.ok_or("missing tree header")?,
            parents,
            author: author.ok_or("missing author header")?,
            committer: committer.ok_or("missing committer header")?,
            extra_headers,
            message: &data[pos..],
        })
    }

    fn find_extra_header(&self, name: &str) -> Option<&[u8]> {
        self.extra_headers
            .iter()
            .find(|(key, _, _)| *key == name.as_bytes())
            .map(|(_, value, _)| value.as_ref())
    }

    /// Returns the signature and the signed data (the commit without the
    /// signature header).
    fn signature(&self, field_name: &str) -> Option<(Vec<u8>, Vec<u8>)> {
        let (_, sig, range) = self
            .extra_headers
            .iter()
            .find(|(key, _, _)| *key == field_name.as_bytes())?;
        let mut signed_data = self.data[..range.start].to_vec();
        signed_data.extend_from_slice(&self.data[range.end..]);
        Some((sig.to_vec(), signed_data))
    }
}

fn find_line_end(data: &[u8], start: usize) -> usize {
    data[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(data.len(), |i| start + i)
}

fn parse_header_oid(hex: &[u8]) -> Result<GritObjectId, String> {
    str::from_utf8(hex)
        .ok()
        .and_then(|hex| GritObjectId::from_hex(hex).ok())
        .ok_or_else(|| format!("invalid object id: {}", String::from_utf8_lossy(hex)))
}

/// Field name of the commit signature header for the given hash algorithm.
fn signature_field_name(hash_algo: HashAlgo) -> &'static str {
    match hash_algo {
        HashAlgo::Sha1 => "gpgsig",
        _ => "gpgsig-sha256",
    }
}

/// Serializes commit headers and message in Git's canonical order.
fn serialize_git_commit(
    tree: &GritObjectId,
    parents: &[GritObjectId],
    author: &[u8],
    committer: &[u8],
    extra_headers: &[(String, Vec<u8>)],
    message: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(b"tree ");
    out.extend_from_slice(tree.to_hex().as_bytes());
    out.push(b'\n');
    for parent in parents {
        out.extend_from_slice(b"parent ");
        out.extend_from_slice(parent.to_hex().as_bytes());
        out.push(b'\n');
    }
    for (name, value) in [("author", author), ("committer", committer)] {
        out.extend_from_slice(name.as_bytes());
        out.push(b' ');
        out.extend_from_slice(value);
        out.push(b'\n');
    }
    for (name, value) in extra_headers {
        out.extend_from_slice(name.as_bytes());
        out.push(b' ');
        let mut lines = value.split_inclusive(|b| *b == b'\n');
        if let Some(line) = lines.next() {
            out.extend_from_slice(line);
        }
        for line in lines {
            out.push(b' ');
            out.extend_from_slice(line);
        }
        if !value.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out.push(b'\n');
    out.extend_from_slice(message);
    out
}

/// Parses the `jj:conflict-labels` header value if present.
fn extract_conflict_labels_from_commit(commit: &ParsedCommit) -> Merge<String> {
    let Some(value) = commit.find_extra_header(JJ_CONFLICT_LABELS_COMMIT_HEADER) else {
        return Merge::resolved(String::new());
    };

    str::from_utf8(value)
        .expect("labels should be valid utf8")
        .split_terminator('\n')
        .map(str::to_owned)
        .collect::<MergeBuilder<_>>()
        .build()
}

/// Parses the `jj:trees` header value if present, otherwise returns the
/// resolved tree ID from Git.
fn extract_root_tree_from_commit(commit: &ParsedCommit) -> Result<Merge<TreeId>, ()> {
    let Some(value) = commit.find_extra_header(JJ_TREES_COMMIT_HEADER) else {
        let tree_id = TreeId::from_bytes(commit.tree.as_bytes());
        return Ok(Merge::resolved(tree_id));
    };

    let hash_len = commit.tree.as_bytes().len();
    let mut tree_ids = SmallVec::new();
    for hex in value.split(|b| *b == b' ') {
        let tree_id = TreeId::try_from_hex(hex).ok_or(())?;
        if tree_id.as_bytes().len() != hash_len {
            return Err(());
        }
        tree_ids.push(tree_id);
    }
    // It is invalid to use `jj:trees` with a non-conflicted tree. If this were
    // allowed, it would be possible to construct a commit which appears to have
    // different contents depending on whether it is viewed using `jj` or `git`.
    if tree_ids.len() == 1 || tree_ids.len() % 2 == 0 {
        return Err(());
    }
    Ok(Merge::from_vec(tree_ids))
}

fn commit_from_git_without_root_parent(
    id: &CommitId,
    data: &[u8],
    hash_algo: HashAlgo,
    is_shallow: bool,
) -> BackendResult<Commit> {
    let commit = ParsedCommit::parse(data).map_err(|err| to_read_object_err(err, id))?;

    // If the git header has a change-id field, we attempt to convert that to a
    // valid JJ Change Id
    let change_id = extract_change_id_from_commit(&commit)
        .unwrap_or_else(|| synthetic_change_id_from_git_commit_id(id));

    // shallow commits don't have parents their parents actually fetched, so we
    // discard them here
    // TODO: This causes issues when a shallow repository is deepened/unshallowed
    let parents = if is_shallow {
        vec![]
    } else {
        commit
            .parents
            .iter()
            .map(|oid| CommitId::from_bytes(oid.as_bytes()))
            .collect_vec()
    };
    // If the commit is a conflict, the conflict labels are stored in a commit
    // header separately from the trees.
    let conflict_labels = extract_conflict_labels_from_commit(&commit);
    // Conflicted commits written before we started using the `jj:trees` header
    // (~March 2024) may have the root trees stored in the extra metadata table
    // instead. For such commits, we'll update the root tree later when we read the
    // extra metadata.
    let root_tree = extract_root_tree_from_commit(&commit)
        .map_err(|()| to_read_object_err("Invalid jj:trees header", id))?;
    // Use lossy conversion as commit message with "mojibake" is still better than
    // nothing.
    // TODO: what should we do with commit.encoding?
    let description = String::from_utf8_lossy(commit.message).into_owned();
    let decode_err = |err: String| to_read_object_err(err, id);
    let author = signature_from_git(commit.author).map_err(decode_err)?;
    let committer = signature_from_git(commit.committer).map_err(decode_err)?;

    // If the commit is signed, extract both the signature and the signed data
    // (which is the commit buffer with the gpgsig header omitted).
    let secure_sig = commit
        .signature(signature_field_name(hash_algo))
        .map(|(sig, data)| SecureSig { data, sig });

    Ok(Commit {
        parents,
        predecessors: vec![],
        // If this commit has associated extra metadata, we may reset this later.
        root_tree,
        conflict_labels,
        change_id,
        description,
        author,
        committer,
        secure_sig,
    })
}

/// Extracts change id from commit headers.
fn extract_change_id_from_commit(commit: &ParsedCommit) -> Option<ChangeId> {
    commit
        .find_extra_header(CHANGE_ID_COMMIT_HEADER)
        .and_then(ChangeId::try_from_reverse_hex)
        .filter(|val| val.as_bytes().len() == CHANGE_ID_LENGTH)
}

/// Deterministically creates a change id based on the commit id
///
/// Used when we get a commit without a change id. The exact algorithm for the
/// computation should not be relied upon.
pub fn synthetic_change_id_from_git_commit_id(id: &CommitId) -> ChangeId {
    // We reverse the bits of the commit id to create the change id. We don't
    // want to use the first bytes unmodified because then it would be ambiguous
    // if a given hash prefix refers to the commit id or the change id. It would
    // have been enough to pick the last 16 bytes instead of the leading 16
    // bytes to address that. We also reverse the bits to make it less likely
    // that users depend on any relationship between the two ids.
    let bytes = id.as_bytes()[id.as_bytes().len() - CHANGE_ID_LENGTH..]
        .iter()
        .rev()
        .map(|b| b.reverse_bits())
        .collect();
    ChangeId::new(bytes)
}

const EMPTY_STRING_PLACEHOLDER: &str = "JJ_EMPTY_STRING";

/// Parses a `name <email> seconds offset` signature line.
fn signature_from_git(raw: &[u8]) -> Result<Signature, String> {
    let line = &raw[..find_line_end(raw, 0)];
    let right_delim = line
        .iter()
        .rposition(|b| *b == b'>')
        .ok_or("Closing '>' not found")?;
    let name_and_email = &line[..right_delim];
    let skip_from_right = name_and_email
        .iter()
        .rev()
        .take_while(|b| **b == b'>')
        .count();
    let left_delim = name_and_email
        .iter()
        .position(|b| *b == b'<')
        .ok_or("Opening '<' not found")?;
    let skip_from_left = line[left_delim..]
        .iter()
        .take_while(|b| **b == b'<')
        .count();
    let name = &line[..left_delim];
    let name = name.strip_suffix(b" ").unwrap_or(name);
    let email = line
        .get(left_delim + skip_from_left..right_delim - skip_from_right)
        .ok_or("Skipped parts run into each other")?;
    let time = &line[right_delim + 1..];

    let decode = |value: &[u8]| {
        if value != EMPTY_STRING_PLACEHOLDER.as_bytes() {
            String::from_utf8_lossy(value).into_owned()
        } else {
            "".to_string()
        }
    };
    let (seconds, offset) = parse_git_time(time).unwrap_or_default();
    Ok(Signature {
        name: decode(name),
        email: decode(email),
        timestamp: Timestamp {
            timestamp: MillisSinceEpoch(seconds * 1000),
            tz_offset: offset.div_euclid(60), // in minutes
        },
    })
}

/// Parses `seconds +hhmm` into seconds since epoch and offset in seconds.
fn parse_git_time(time: &[u8]) -> Option<(i64, i32)> {
    let time = str::from_utf8(time).ok()?;
    let mut parts = time.split_ascii_whitespace();
    let seconds = parts.next()?.parse().ok()?;
    let offset = parts.next().unwrap_or("+0000");
    let (sign, digits) = match offset.as_bytes().first()? {
        b'+' => (1, &offset[1..]),
        b'-' => (-1, &offset[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    Some((seconds, sign * (hours * 3600 + minutes * 60)))
}

/// Formats a signature as `name <email> seconds offset`.
///
/// `seconds` overrides the signature's timestamp (in seconds since epoch).
fn signature_to_git(signature: &Signature, seconds: i64) -> Result<Vec<u8>, String> {
    // git does not support empty names or emails
    let name = if !signature.name.is_empty() {
        &signature.name
    } else {
        EMPTY_STRING_PLACEHOLDER
    };
    let email = if !signature.email.is_empty() {
        &signature.email
    } else {
        EMPTY_STRING_PLACEHOLDER
    };
    if [name, email]
        .iter()
        .any(|token| token.contains(['<', '>', '\n']))
    {
        return Err("Signature name or email must not contain '<', '>' or \\n".to_owned());
    }
    let offset = signature.timestamp.tz_offset * 60; // in seconds
    let hours = offset.unsigned_abs() / 3600;
    let minutes = (offset.unsigned_abs() % 3600) / 60;
    if hours > 99 {
        return Err("Cannot represent offsets larger than +-9900".to_owned());
    }
    let sign = if offset < 0 { '-' } else { '+' };
    Ok(format!("{name} <{email}> {seconds} {sign}{hours:02}{minutes:02}").into_bytes())
}

fn serialize_extras(commit: &Commit) -> Vec<u8> {
    let mut proto = crate::protos::git_store::Commit {
        change_id: commit.change_id.to_bytes(),
        ..Default::default()
    };
    proto.uses_tree_conflict_format = true;
    for predecessor in &commit.predecessors {
        proto.predecessors.push(predecessor.to_bytes());
    }
    proto.encode_to_vec()
}

fn deserialize_extras(commit: &mut Commit, bytes: &[u8]) {
    let proto = crate::protos::git_store::Commit::decode(bytes).unwrap();
    if !proto.change_id.is_empty() {
        commit.change_id = ChangeId::new(proto.change_id);
    }
    if commit.root_tree.is_resolved()
        && proto.uses_tree_conflict_format
        && !proto.root_tree.is_empty()
    {
        let merge_builder: MergeBuilder<_> = proto
            .root_tree
            .iter()
            .map(|id_bytes| TreeId::from_bytes(id_bytes))
            .collect();
        commit.root_tree = merge_builder.build();
    }
    for predecessor in &proto.predecessors {
        commit.predecessors.push(CommitId::from_bytes(predecessor));
    }
}

/// Returns `RefUpdate` that will create a ref in `refs/jj/keep`. Used for
/// preventing GC of commits we create.
fn to_no_gc_ref_update(id: &CommitId) -> RefUpdate {
    let oid = GritObjectId::from_bytes(id.as_bytes()).unwrap();
    RefUpdate {
        name: format!("{NO_GC_REF_NAMESPACE}{id}"),
        new_value: Some(RawRef::Direct(oid)),
        // The ref name is derived from the id, so an existing ref already
        // points to the same commit.
        expected: Expected::Any,
        reflog: None,
        flags: RefUpdateFlags::default(),
    }
}

fn to_ref_deletion(name: String, oid: GritObjectId) -> RefUpdate {
    RefUpdate {
        name,
        new_value: None,
        expected: Expected::Oid(oid),
        reflog: None,
        flags: RefUpdateFlags::default(),
    }
}

/// Applies the given ref updates in one transaction.
fn edit_references(
    repo: &grit_lib::repo::Repository,
    updates: impl IntoIterator<Item = RefUpdate>,
) -> BackendResult<()> {
    crate::git_grit::edit_refs(repo, updates).map_err(|err| BackendError::Other(err.into()))
}

/// Recreates `refs/jj/keep` refs for the `new_heads`, and removes the other
/// unreachable and non-head refs.
fn recreate_no_gc_refs(
    git_repo: &grit_lib::repo::Repository,
    new_heads: &HashSet<&CommitId>,
    keep_newer: SystemTime,
) -> BackendResult<()> {
    // Calculate diff between existing no-gc refs and new heads.
    let mut no_gc_refs_to_keep_count: usize = 0;
    let mut no_gc_refs_to_delete: Vec<(String, GritObjectId)> = Vec::new();
    let mut no_gc_refs = Vec::new();
    git_repo
        .refs()
        .for_each_ref(NO_GC_REF_NAMESPACE, &mut |entry| {
            no_gc_refs.push((entry.name.clone(), entry.value.clone()));
            std::ops::ControlFlow::Continue(())
        })
        .map_err(|err| BackendError::Other(err.into()))?;
    for (name, value) in no_gc_refs {
        let RawRef::Direct(oid) = value else {
            return Err(BackendError::Other(
                format!("Symbolic no-gc ref found: {name}").into(),
            ));
        };
        let id = CommitId::from_bytes(oid.as_bytes());
        let name_good = name[NO_GC_REF_NAMESPACE.len()..] == id.hex();
        if new_heads.contains(&id) && name_good {
            no_gc_refs_to_keep_count += 1;
            continue;
        }
        // Check timestamp of loose ref, but this is still racy on re-import
        // because:
        // - existing packed ref won't be demoted to loose ref
        // - existing loose ref won't be touched
        //
        // TODO: might be better to switch to a dummy merge, where new no-gc ref
        // will always have a unique name. Doing that with the current
        // ref-per-head strategy would increase the number of the no-gc refs.
        // https://github.com/jj-vcs/jj/pull/2659#issuecomment-1837057782
        let loose_ref_path = git_repo.common_git_dir().join(&name);
        if let Ok(metadata) = loose_ref_path.metadata() {
            let mtime = metadata.modified().expect("unsupported platform?");
            if mtime > keep_newer {
                tracing::trace!(?name, "not deleting new");
                no_gc_refs_to_keep_count += 1;
                continue;
            }
        }
        // Also deletes no-gc ref of random name created by old jj.
        tracing::trace!(?name, ?name_good, "will delete");
        no_gc_refs_to_delete.push((name, oid));
    }
    tracing::info!(
        new_heads_count = new_heads.len(),
        no_gc_refs_to_keep_count,
        no_gc_refs_to_delete_count = no_gc_refs_to_delete.len(),
        "collected reachable refs"
    );

    // It's slow to delete packed refs one by one, so update refs all at once.
    let ref_edits = itertools::chain(
        no_gc_refs_to_delete
            .into_iter()
            .map(|(name, oid)| to_ref_deletion(name, oid)),
        new_heads.iter().copied().map(to_no_gc_ref_update),
    );
    edit_references(git_repo, ref_edits)
}

fn run_git_gc(program: &OsStr, git_dir: &Path, keep_newer: SystemTime) -> Result<(), GitGcError> {
    let keep_newer = keep_newer
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default(); // underflow
    let mut git = Command::new(program);
    git.arg("--git-dir=.") // turn off discovery
        .arg("gc")
        .arg(format!("--prune=@{} +0000", keep_newer.as_secs()));
    // Don't specify it by GIT_DIR/--git-dir. On Windows, the path could be
    // canonicalized as UNC path, which wouldn't be supported by git.
    git.current_dir(git_dir);
    // TODO: pass output to UI layer instead of printing directly here
    tracing::info!(?git, "running git gc");
    let status = git.status().map_err(GitGcError::GcCommand)?;
    tracing::info!(?status, "git gc exited");
    if !status.success() {
        return Err(GitGcError::GcCommandErrorStatus(status));
    }
    Ok(())
}

fn validate_git_object_id(
    repo: &grit_lib::repo::Repository,
    id: &impl ObjectId,
) -> BackendResult<GritObjectId> {
    let expected_algo = repo.odb().hash_algo();
    match GritObjectId::from_bytes(id.as_bytes()) {
        Ok(oid) if oid.algo() == expected_algo => Ok(oid),
        _ => Err(BackendError::InvalidHashLength {
            expected: expected_algo.len(),
            actual: id.as_bytes().len(),
            object_type: id.object_type(),
            hash: id.hex(),
        }),
    }
}

fn not_found_err(id: &impl ObjectId) -> BackendError {
    BackendError::ObjectNotFound {
        object_type: id.object_type(),
        hash: id.hex(),
        source: None,
    }
}

fn to_read_object_err(
    err: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    id: &impl ObjectId,
) -> BackendError {
    BackendError::ReadObject {
        object_type: id.object_type(),
        hash: id.hex(),
        source: err.into(),
    }
}

fn to_invalid_utf8_err(source: Utf8Error, id: &impl ObjectId) -> BackendError {
    BackendError::InvalidUtf8 {
        object_type: id.object_type(),
        hash: id.hex(),
        source,
    }
}

fn import_extra_metadata_entries_from_heads(
    git_repo: &grit_lib::repo::Repository,
    mut_table: &mut MutableTable,
    _table_lock: &FileLock,
    head_ids: &HashSet<&CommitId>,
    shallow_roots: &[CommitId],
) -> BackendResult<()> {
    let mut work_ids = head_ids
        .iter()
        .filter(|&id| mut_table.get_value(id.as_bytes()).is_none())
        .map(|&id| id.clone())
        .collect_vec();
    let hash_algo = git_repo.odb().hash_algo();
    while let Some(id) = work_ids.pop() {
        let data = read_object_of_kind(git_repo, &id, ObjectKind::Commit)?;
        let is_shallow = shallow_roots.contains(&id);
        // TODO(#1624): Should we read the root tree here and check if it has a
        // `.jjconflict-...` entries? That could happen if the user used `git` to e.g.
        // change the description of a commit with tree-level conflicts.
        let commit = commit_from_git_without_root_parent(&id, &data, hash_algo, is_shallow)?;
        mut_table.add_entry(id.to_bytes(), serialize_extras(&commit));
        work_ids.extend(
            commit
                .parents
                .into_iter()
                .filter(|id| mut_table.get_value(id.as_bytes()).is_none()),
        );
    }
    Ok(())
}

impl Debug for GitBackend {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        f.debug_struct("GitBackend")
            .field("path", &self.git_repo_path())
            .finish()
    }
}

#[async_trait]
impl Backend for GitBackend {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn commit_id_length(&self) -> usize {
        self.grit_repo.odb().hash_algo().len()
    }

    fn change_id_length(&self) -> usize {
        CHANGE_ID_LENGTH
    }

    fn root_commit_id(&self) -> &CommitId {
        &self.root_commit_id
    }

    fn root_change_id(&self) -> &ChangeId {
        &self.root_change_id
    }

    fn empty_tree_id(&self) -> &TreeId {
        &self.empty_tree_id
    }

    fn concurrency(&self) -> usize {
        1
    }

    async fn read_file(
        &self,
        _path: &RepoPath,
        id: &FileId,
    ) -> BackendResult<Pin<Box<dyn AsyncRead + Send>>> {
        let data = self.read_file_sync(id)?;
        Ok(Box::pin(Cursor::new(data)))
    }

    async fn write_file(
        &self,
        _path: &RepoPath,
        contents: &mut (dyn AsyncRead + Send + Unpin),
    ) -> BackendResult<FileId> {
        let mut bytes = Vec::new();
        contents.read_to_end(&mut bytes).await.unwrap();

        let oid = self.write_blob(&bytes, "file")?;
        Ok(FileId::new(oid.as_bytes().to_vec()))
    }

    async fn read_symlink(&self, _path: &RepoPath, id: &SymlinkId) -> BackendResult<String> {
        let data = read_object_of_kind(&self.grit_repo, id, ObjectKind::Blob)?;
        let target =
            String::from_utf8(data).map_err(|err| to_invalid_utf8_err(err.utf8_error(), id))?;
        Ok(target)
    }

    async fn write_symlink(&self, _path: &RepoPath, target: &str) -> BackendResult<SymlinkId> {
        let oid = self.write_blob(target.as_bytes(), "symlink")?;
        Ok(SymlinkId::new(oid.as_bytes().to_vec()))
    }

    async fn read_copy(&self, _id: &CopyId) -> BackendResult<CopyHistory> {
        Err(BackendError::Unsupported(
            "The Git backend doesn't support tracked copies yet".to_string(),
        ))
    }

    async fn write_copy(&self, _contents: &CopyHistory) -> BackendResult<CopyId> {
        Err(BackendError::Unsupported(
            "The Git backend doesn't support tracked copies yet".to_string(),
        ))
    }

    async fn get_related_copies(&self, _copy_id: &CopyId) -> BackendResult<Vec<RelatedCopy>> {
        Err(BackendError::Unsupported(
            "The Git backend doesn't support tracked copies yet".to_string(),
        ))
    }

    async fn read_tree(&self, _path: &RepoPath, id: &TreeId) -> BackendResult<Tree> {
        if id == &self.empty_tree_id {
            return Ok(Tree::default());
        }

        let data = read_object_of_kind(&self.grit_repo, id, ObjectKind::Tree)?;
        let oid_len = self.grit_repo.odb().hash_algo().len();
        let mut entries: Vec<_> = TreeRefIter::with_oid_len(&data, oid_len)
            .map(|entry| -> BackendResult<_> {
                let entry = entry.map_err(|err| to_read_object_err(err, id))?;
                let name = RepoPathComponentBuf::new(
                    str::from_utf8(entry.name).map_err(|err| to_invalid_utf8_err(err, id))?,
                )
                .map_err(|err| to_read_object_err(err, id))?;
                let value = match TreeEntryKind::from_mode(entry.mode) {
                    TreeEntryKind::Tree => {
                        let id = TreeId::from_bytes(entry.oid.as_bytes());
                        TreeValue::Tree(id)
                    }
                    TreeEntryKind::Blob => {
                        let id = FileId::from_bytes(entry.oid.as_bytes());
                        TreeValue::File {
                            id,
                            executable: false,
                            copy_id: CopyId::placeholder(),
                        }
                    }
                    TreeEntryKind::BlobExecutable => {
                        let id = FileId::from_bytes(entry.oid.as_bytes());
                        TreeValue::File {
                            id,
                            executable: true,
                            copy_id: CopyId::placeholder(),
                        }
                    }
                    TreeEntryKind::Link => {
                        let id = SymlinkId::from_bytes(entry.oid.as_bytes());
                        TreeValue::Symlink(id)
                    }
                    TreeEntryKind::Commit => {
                        let id = CommitId::from_bytes(entry.oid.as_bytes());
                        TreeValue::GitSubmodule(id)
                    }
                };
                Ok((name, value))
            })
            .try_collect()?;
        // While Git tree entries are sorted, the rule is slightly different.
        // Directory names are sorted as if they had trailing "/".
        if !entries.is_sorted_by_key(|(name, _)| name) {
            entries.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        }
        Ok(Tree::from_sorted_entries(entries))
    }

    async fn write_tree(&self, _path: &RepoPath, contents: &Tree) -> BackendResult<TreeId> {
        let entries = contents
            .entries()
            .map(|entry| {
                let name = entry.name().as_internal_str().as_bytes().to_vec();
                let (kind, id) = match entry.value() {
                    TreeValue::File {
                        id,
                        executable: false,
                        copy_id: _, // TODO: Use the value
                    } => (TreeEntryKind::Blob, id.as_bytes()),
                    TreeValue::File {
                        id,
                        executable: true,
                        copy_id: _, // TODO: Use the value
                    } => (TreeEntryKind::BlobExecutable, id.as_bytes()),
                    TreeValue::Symlink(id) => (TreeEntryKind::Link, id.as_bytes()),
                    TreeValue::Tree(id) => (TreeEntryKind::Tree, id.as_bytes()),
                    TreeValue::GitSubmodule(id) => (TreeEntryKind::Commit, id.as_bytes()),
                };
                GritTreeEntry {
                    mode: kind.mode(),
                    name,
                    oid: GritObjectId::from_bytes(id).unwrap(),
                }
            })
            .collect_vec();
        let oid = write_tree_entries(&self.grit_repo, entries)?;
        Ok(TreeId::from_bytes(oid.as_bytes()))
    }

    #[tracing::instrument(skip(self))]
    async fn read_commit(&self, id: &CommitId) -> BackendResult<Commit> {
        if *id == self.root_commit_id {
            return Ok(make_root_commit(
                self.root_change_id().clone(),
                self.empty_tree_id.clone(),
            ));
        }

        let mut commit = {
            let data = read_object_of_kind(&self.grit_repo, id, ObjectKind::Commit)?;
            let is_shallow = self.shallow_root_ids()?.contains(id);
            let hash_algo = self.grit_repo.odb().hash_algo();
            commit_from_git_without_root_parent(id, &data, hash_algo, is_shallow)?
        };
        if commit.parents.is_empty() {
            commit.parents.push(self.root_commit_id.clone());
        }

        let table = self.cached_extra_metadata_table()?;
        if let Some(extras) = table.get_value(id.as_bytes()) {
            deserialize_extras(&mut commit, extras);
        } else {
            // TODO: Remove this hack and map to ObjectNotFound error if we're sure that
            // there are no reachable ancestor commits without extras metadata. Git commits
            // imported by jj < 0.8.0 might not have extras (#924).
            // https://github.com/jj-vcs/jj/issues/2343
            tracing::info!("unimported Git commit found");
            self.import_head_commits([id])?;
            let table = self.cached_extra_metadata_table()?;
            let extras = table.get_value(id.as_bytes()).unwrap();
            deserialize_extras(&mut commit, extras);
        }
        Ok(commit)
    }

    async fn write_commit(
        &self,
        mut contents: Commit,
        mut sign_with: Option<&mut SigningFn>,
    ) -> BackendResult<(CommitId, Commit)> {
        assert!(contents.secure_sig.is_none(), "commit.secure_sig was set");

        let repo = &self.grit_repo;
        let hash_algo = repo.odb().hash_algo();
        let tree_ids = &contents.root_tree;
        let git_tree_id = match tree_ids.as_resolved() {
            Some(tree_id) => validate_git_object_id(repo, tree_id)?,
            None => write_tree_conflict(repo, tree_ids)?,
        };
        let to_signature_err = |err: String| BackendError::WriteObject {
            object_type: "commit",
            source: err.into(),
        };
        let author_seconds = contents.author.timestamp.timestamp.0.div_euclid(1000);
        let author =
            signature_to_git(&contents.author, author_seconds).map_err(to_signature_err)?;
        let mut committer_seconds = contents.committer.timestamp.timestamp.0.div_euclid(1000);
        let message = &contents.description;
        if contents.parents.is_empty() {
            return Err(BackendError::Other(
                "Cannot write a commit with no parents".into(),
            ));
        }
        // Git doesn't have a root commit, so we don't write it as a parent in the Git
        // commit. Since the root commit is an ancestor of every commit, we also leave
        // it out of the parents of a merge commit.
        contents.parents.retain(|id| *id != self.root_commit_id);
        let mut parents = Vec::new();
        for parent_id in &contents.parents {
            parents.push(validate_git_object_id(repo, parent_id)?);
        }
        // Match what read_commit() returns for a Git commit without parents.
        if contents.parents.is_empty() {
            contents.parents.push(self.root_commit_id.clone());
        }
        let mut extra_headers: Vec<(String, Vec<u8>)> = vec![];
        if !contents.conflict_labels.is_resolved() {
            // Labels cannot contain '\n' since we use it as a separator in the header.
            assert!(
                contents
                    .conflict_labels
                    .iter()
                    .all(|label| !label.contains('\n'))
            );
            let mut joined_with_newlines = contents.conflict_labels.iter().join("\n");
            joined_with_newlines.push('\n');
            extra_headers.push((
                JJ_CONFLICT_LABELS_COMMIT_HEADER.into(),
                joined_with_newlines.into(),
            ));
        }
        if !tree_ids.is_resolved() {
            let value = tree_ids.iter().map(|id| id.hex()).join(" ");
            extra_headers.push((JJ_TREES_COMMIT_HEADER.into(), value.into()));
        }
        if self.write_change_id_header {
            extra_headers.push((
                CHANGE_ID_COMMIT_HEADER.into(),
                contents.change_id.reverse_hex().into(),
            ));
        }

        if tree_ids.iter().any(|id| id == &self.empty_tree_id) {
            let tree_id = write_object(repo, ObjectKind::Tree, b"", "tree")?;
            assert!(TreeId::from_bytes(tree_id.as_bytes()) == self.empty_tree_id);
        }

        let extras = serialize_extras(&contents);

        // If two writers write commits of the same id with different metadata, they
        // will both succeed and the metadata entries will be "merged" later. Since
        // metadata entry is keyed by the commit id, one of the entries would be lost.
        // To prevent such race condition locally, we extend the scope covered by the
        // table lock. This is still racy if multiple machines are involved and the
        // repository is rsync-ed.
        let (table, table_lock) = self.read_extra_metadata_table_locked()?;
        let id = loop {
            let committer = signature_to_git(&contents.committer, committer_seconds)
                .map_err(to_signature_err)?;
            let mut headers = extra_headers.clone();
            if let Some(sign) = &mut sign_with {
                let data = serialize_git_commit(
                    &git_tree_id,
                    &parents,
                    &author,
                    &committer,
                    &headers,
                    message.as_bytes(),
                );
                let sig = sign(&data).map_err(|err| BackendError::WriteObject {
                    object_type: "commit",
                    source: Box::new(err),
                })?;
                headers.push((signature_field_name(hash_algo).into(), sig.clone()));
                contents.secure_sig = Some(SecureSig { data, sig });
            }

            let data = serialize_git_commit(
                &git_tree_id,
                &parents,
                &author,
                &committer,
                &headers,
                message.as_bytes(),
            );
            let git_id = write_object(repo, ObjectKind::Commit, &data, "commit")?;

            match table.get_value(git_id.as_bytes()) {
                Some(existing_extras) if existing_extras != extras => {
                    // It's possible a commit already exists with the same
                    // commit id but different change id. Adjust the timestamp
                    // until this is no longer the case.
                    //
                    // For example, this can happen when rebasing duplicate
                    // commits, https://github.com/jj-vcs/jj/issues/694.
                    //
                    // `jj` resets the committer timestamp to the current
                    // timestamp whenever it rewrites a commit. So, it's
                    // unlikely for the timestamp to be 0 even if the original
                    // commit had its timestamp set to 0. Moreover, we test that
                    // a commit with a negative timestamp can still be written
                    // and read back by `jj`.
                    committer_seconds -= 1;
                }
                _ => break CommitId::from_bytes(git_id.as_bytes()),
            }
        };

        // Everything up to this point had no permanent effect on the repo except
        // GC-able objects
        edit_references(&self.grit_repo, [to_no_gc_ref_update(&id)])?;

        // Update the signature to match the one that was actually written to the object
        // store
        contents.committer.timestamp.timestamp = MillisSinceEpoch(committer_seconds * 1000);
        let mut mut_table = table.start_mutation();
        mut_table.add_entry(id.to_bytes(), extras);
        self.save_extra_metadata_table(mut_table, &table_lock)?;
        Ok((id, contents))
    }

    fn get_copy_records(
        &self,
        paths: Option<&[RepoPathBuf]>,
        root_id: &CommitId,
        head_id: &CommitId,
    ) -> BackendResult<BoxStream<'_, BackendResult<CopyRecord>>> {
        let root_tree = self.read_tree_id_for_commit(root_id)?;
        let head_tree = self.read_tree_id_for_commit(head_id)?;

        let hash_algo = self.grit_repo.odb().hash_algo();
        let empty_blob_id = grit_lib::objects::hash::hash_object(hash_algo, ObjectKind::Blob, b"")
            .map_err(|err| BackendError::Other(err.into()))?;
        let change_to_copy_record = |change: DiffEntry| -> BackendResult<Option<CopyRecord>> {
            if !matches!(change.status, DiffStatus::Renamed | DiffStatus::Copied) {
                return Ok(None);
            }
            // TODO: Renamed symlinks cannot be returned because CopyRecord
            // expects `source_file: FileId`.
            let is_blob = |mode: &str| matches!(mode, "100644" | "100755");
            if !is_blob(&change.old_mode) || !is_blob(&change.new_mode) {
                return Ok(None);
            }
            // Empty files are too common to say anything about copies.
            if change.old_oid == empty_blob_id {
                return Ok(None);
            }
            let (Some(source), Some(dest)) = (change.old_path, change.new_path) else {
                return Ok(None);
            };

            let target = RepoPathBuf::from_internal_string(dest)
                .map_err(|err| to_read_object_err(err, head_id))?;
            if !paths.is_none_or(|paths| paths.contains(&target)) {
                return Ok(None);
            }

            Ok(Some(CopyRecord {
                target,
                target_commit: head_id.clone(),
                source: RepoPathBuf::from_internal_string(source)
                    .map_err(|err| to_read_object_err(err, root_id))?,
                source_file: FileId::from_bytes(change.old_oid.as_bytes()),
                source_commit: root_id.clone(),
            }))
        };

        let odb = self.grit_repo.odb();
        let mut changes = grit_lib::diff::diff_trees(odb, Some(&root_tree), Some(&head_tree), "")
            .map_err(|err| BackendError::Other(err.into()))?;
        // Unlike `git diff -C`, which copies from the preimage of modified
        // files, jj has always used their new contents as copy sources.
        for change in &mut changes {
            if change.status == DiffStatus::Modified {
                change.old_oid = change.new_oid;
                change.old_mode.clone_from(&change.new_mode);
            }
        }
        // Renames, and copies from the set of modified files, at 50%
        // similarity.
        let changes = grit_lib::diff::detect_copies(odb, None, changes, 50, false, &[])
            .map_err(|err| BackendError::Other(err.into()))?;
        let records: Vec<BackendResult<CopyRecord>> = changes
            .into_iter()
            .filter_map(|change| change_to_copy_record(change).transpose())
            .collect();
        Ok(futures::stream::iter(records).boxed())
    }

    /// Perform garbage collection.
    ///
    /// All commits found in the `index` won't be removed. In addition to that,
    /// objects created after `keep_newer` will be preserved. This mitigates a
    /// risk of deleting new commits created concurrently by another process.
    #[tracing::instrument(skip(self, new_heads))]
    async fn gc(&self, new_heads: &[CommitId], keep_newer: SystemTime) -> BackendResult<()> {
        let new_heads = new_heads
            .iter()
            .filter(|id| *id != &self.root_commit_id)
            .collect();
        recreate_no_gc_refs(&self.grit_repo, &new_heads, keep_newer)?;

        // No locking is needed since we aren't going to add new "commits".
        let table = self.cached_extra_metadata_table()?;
        // TODO: remove unreachable entries from extras table if segment file
        // mtime <= keep_newer? (it won't be consistent with no-gc refs
        // preserved by the keep_newer timestamp though)
        self.extra_metadata_store
            .gc(&table, keep_newer)
            .map_err(|err| BackendError::Other(err.into()))?;

        run_git_gc(
            self.git_executable.as_ref(),
            self.git_repo_path(),
            keep_newer,
        )
        .map_err(|err| BackendError::Other(err.into()))?;
        // Since "git gc" will move loose refs into packed refs, in-memory
        // packed-refs cache should be invalidated without relying on mtime.
        self.lock_git_repo().refs.force_refresh_packed_buffer().ok();
        Ok(())
    }
}

/// Write a tree conflict as a special tree with `.jjconflict-base-N` and
/// `.jjconflict-side-N` subtrees. This ensure that the parts are not GC'd.
/// Also includes a `JJ-CONFLICT-README` file explaining why these trees are
/// present. The rest of the tree is copied from the first term of the conflict,
/// which prevents editors with Git support from highlighting all files as new.
fn write_tree_conflict(
    repo: &grit_lib::repo::Repository,
    conflict: &Merge<TreeId>,
) -> BackendResult<GritObjectId> {
    let mut entries = itertools::chain(
        conflict
            .removes()
            .enumerate()
            .map(|(i, tree_id)| (format!(".jjconflict-base-{i}"), tree_id)),
        conflict
            .adds()
            .enumerate()
            .map(|(i, tree_id)| (format!(".jjconflict-side-{i}"), tree_id)),
    )
    .map(|(name, tree_id)| GritTreeEntry {
        mode: TreeEntryKind::Tree.mode(),
        name: name.into_bytes(),
        oid: GritObjectId::from_bytes(tree_id.as_bytes()).unwrap(),
    })
    .collect_vec();
    let readme_id = write_object(
        repo,
        ObjectKind::Blob,
        br#"This commit was made by jj, https://jj-vcs.dev/.
The commit contains file conflicts, and therefore looks wrong when used with
plain Git or other tools that are unfamiliar with jj.

The .jjconflict-* directories represent the different inputs to the conflict.
For details, see
https://docs.jj-vcs.dev/latest/git-compatibility/#format-mapping-details

If you see this file in your working copy, it probably means that you used a
regular `git` command to check out a conflicted commit. Use `jj abandon` to
recover.
"#,
        "blob",
    )?;
    entries.push(GritTreeEntry {
        mode: TreeEntryKind::Blob.mode(),
        name: JJ_CONFLICT_README_FILE_NAME.into(),
        oid: readme_id,
    });
    let first_tree_id = conflict.first();
    let first_tree = read_object_of_kind(repo, first_tree_id, ObjectKind::Tree)?;
    let oid_len = repo.odb().hash_algo().len();
    for entry in TreeRefIter::with_oid_len(&first_tree, oid_len) {
        let entry = entry.map_err(|err| to_read_object_err(err, first_tree_id))?;
        if !entry.name.starts_with(b".jjconflict")
            && entry.name != JJ_CONFLICT_README_FILE_NAME.as_bytes()
        {
            entries.push(entry.to_owned());
        }
    }
    write_tree_entries(repo, entries)
}

/// Kind of a Git tree entry, derived from its mode the way Git does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TreeEntryKind {
    Tree,
    Blob,
    BlobExecutable,
    Link,
    Commit,
}

impl TreeEntryKind {
    fn from_mode(mode: u32) -> Self {
        match mode & 0o170000 {
            0o040000 => Self::Tree,
            0o120000 => Self::Link,
            0o160000 => Self::Commit,
            _ if mode & 0o111 != 0 => Self::BlobExecutable,
            _ => Self::Blob,
        }
    }

    fn mode(self) -> u32 {
        match self {
            Self::Tree => 0o040000,
            Self::Blob => 0o100644,
            Self::BlobExecutable => 0o100755,
            Self::Link => 0o120000,
            Self::Commit => 0o160000,
        }
    }
}

/// Writes a tree object, sorting the entries in Git's tree order.
fn write_tree_entries(
    repo: &grit_lib::repo::Repository,
    mut entries: Vec<GritTreeEntry>,
) -> BackendResult<GritObjectId> {
    let is_tree = |entry: &GritTreeEntry| entry.mode == TreeEntryKind::Tree.mode();
    entries.sort_unstable_by(|a, b| tree_entry_cmp(&a.name, is_tree(a), &b.name, is_tree(b)));
    let data = serialize_tree(&entries);
    write_object(repo, ObjectKind::Tree, &data, "tree")
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use gix::date::parse::TimeBuf;
    use gix::objs::WriteTo as _;
    use indoc::indoc;
    use test_case::test_case;

    use super::*;
    use crate::config::StackedConfig;
    use crate::content_hash::blake2b_hash;
    use crate::hex_util;
    use crate::tests::TestResult;
    use crate::tests::new_temp_dir;

    const GIT_USER: &str = "Someone";
    const GIT_EMAIL: &str = "someone@example.com";

    fn git_config() -> Vec<bstr::BString> {
        vec![
            format!("user.name = {GIT_USER}").into(),
            format!("user.email = {GIT_EMAIL}").into(),
            "init.defaultBranch = master".into(),
        ]
    }

    fn open_options() -> gix::open::Options {
        gix::open::Options::isolated()
            .config_overrides(git_config())
            .strict_config(true)
    }

    fn git_init(directory: impl AsRef<Path>, object_hash: gix::hash::Kind) -> gix::Repository {
        gix::ThreadSafeRepository::init_opts(
            directory,
            gix::create::Kind::WithWorktree,
            gix::create::Options {
                object_hash: Some(object_hash),
                ..Default::default()
            },
            open_options(),
        )
        .unwrap()
        .to_thread_local()
    }

    #[test]
    fn open_git_repo_at_workdir() -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path().join("store");
        fs::create_dir(&store_path)?;

        let git_repo_path = temp_dir.path().join("git1");
        let git_repo = git_init(&git_repo_path, gix::hash::Kind::default());
        let other_git_repo_path = temp_dir.path().join("git2");
        let _other_git_repo = git_init(&other_git_repo_path, gix::hash::Kind::default());

        let worktree_dir = temp_dir.path().join("git1-wt");
        let output = Command::new("git")
            .args(["worktree", "add", "--orphan"])
            .arg(&worktree_dir)
            .current_dir(&git_repo_path)
            .output()?;
        assert!(output.status.success(), "{output:?}");

        let backend = GitBackend::init_external(&settings, &store_path, git_repo.path())?;

        assert_matches!(
            backend.open_git_repo_at_workdir(&git_repo_path),
            Ok(repo) if repo.workdir() == Some(backend.git_repo().workdir().unwrap())
        );
        assert_matches!(
            backend.open_git_repo_at_workdir(&worktree_dir),
            Ok(repo) if repo.workdir() == Some(worktree_dir.as_ref())
        );
        assert_matches!(
            backend.open_git_repo_at_workdir(&temp_dir.path().join("unknown")),
            Err(GitRepoAtWorkdirError::NotFound { .. })
        );
        assert_matches!(
            backend.open_git_repo_at_workdir(&other_git_repo_path),
            Err(GitRepoAtWorkdirError::Unrelated { .. })
        );

        Ok(())
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn read_plain_git_commit(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path();
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(git_repo_path, object_hash);

        // Add a commit with some files in
        let blob1 = git_repo.write_blob(b"content1")?.detach();
        let blob2 = git_repo.write_blob(b"normal")?.detach();
        let mut dir_tree_editor = git_repo.empty_tree().edit()?;
        dir_tree_editor.upsert("normal", gix::object::tree::EntryKind::Blob, blob1)?;
        dir_tree_editor.upsert("symlink", gix::object::tree::EntryKind::Link, blob2)?;
        let dir_tree_id = dir_tree_editor.write()?.detach();
        let mut root_tree_builder = git_repo.empty_tree().edit()?;
        root_tree_builder.upsert("dir", gix::object::tree::EntryKind::Tree, dir_tree_id)?;
        let root_tree_id = root_tree_builder.write()?.detach();
        let git_author = gix::actor::Signature {
            name: "git author".into(),
            email: "git.author@example.com".into(),
            time: gix::date::Time::new(1000, 60 * 60),
        };
        let git_committer = gix::actor::Signature {
            name: "git committer".into(),
            email: "git.committer@example.com".into(),
            time: gix::date::Time::new(2000, -480 * 60),
        };
        let git_commit_id = git_repo
            .commit_as(
                git_committer.to_ref(&mut TimeBuf::default()),
                git_author.to_ref(&mut TimeBuf::default()),
                "refs/heads/dummy",
                "git commit message",
                root_tree_id,
                [] as [gix::ObjectId; 0],
            )?
            .detach();
        git_repo.find_reference("refs/heads/dummy")?.delete()?;
        // The change id is the leading reverse bits of the commit id
        let (commit_id, change_id) = match object_hash {
            gix::hash::Kind::Sha1 => (
                CommitId::from_hex("efdcea5ca4b3658149f899ca7feee6876d077263"),
                ChangeId::from_hex("c64ee0b6e16777fe53991f9281a6cd25"),
            ),
            gix::hash::Kind::Sha256 => (
                CommitId::from_hex(
                    "64366022e4938d697015b775945be93aea6d3fc221feeaf7c516420262e3fa54",
                ),
                ChangeId::from_hex("2a5fc746404268a3ef577f8443fcb657"),
            ),
            _ => unreachable!(),
        };
        // Check that the git commit above got the hash we expect
        assert_eq!(
            git_commit_id.as_bytes(),
            commit_id.as_bytes(),
            "{git_commit_id:?} vs {commit_id:?}"
        );

        // Add an empty commit on top
        let git_commit_id2 = git_repo
            .commit_as(
                git_committer.to_ref(&mut TimeBuf::default()),
                git_author.to_ref(&mut TimeBuf::default()),
                "refs/heads/dummy2",
                "git commit message 2",
                root_tree_id,
                [git_commit_id],
            )?
            .detach();
        git_repo.find_reference("refs/heads/dummy2")?.delete()?;
        let commit_id2 = CommitId::from_bytes(git_commit_id2.as_bytes());

        let backend = GitBackend::init_external(&settings, store_path, git_repo.path())?;

        // Import the head commit and its ancestors
        backend.import_head_commits([&commit_id2])?;
        // Ref should be created only for the head commit
        let git_refs = backend
            .git_repo()
            .references()?
            .prefixed("refs/jj/keep/")?
            .map(|git_ref| git_ref.unwrap().id().detach())
            .collect_vec();
        assert_eq!(git_refs, vec![git_commit_id2]);

        let commit = backend.read_commit(&commit_id).block_on()?;
        assert_eq!(&commit.change_id, &change_id);
        assert_eq!(
            commit.parents,
            vec![CommitId::from_bytes(object_hash.null_ref().as_bytes())]
        );
        assert_eq!(commit.predecessors, vec![]);
        assert_eq!(
            commit.root_tree,
            Merge::resolved(TreeId::from_bytes(root_tree_id.as_bytes()))
        );
        assert_eq!(commit.description, "git commit message");
        assert_eq!(commit.author.name, "git author");
        assert_eq!(commit.author.email, "git.author@example.com");
        assert_eq!(
            commit.author.timestamp.timestamp,
            MillisSinceEpoch(1000 * 1000)
        );
        assert_eq!(commit.author.timestamp.tz_offset, 60);
        assert_eq!(commit.committer.name, "git committer");
        assert_eq!(commit.committer.email, "git.committer@example.com");
        assert_eq!(
            commit.committer.timestamp.timestamp,
            MillisSinceEpoch(2000 * 1000)
        );
        assert_eq!(commit.committer.timestamp.tz_offset, -480);

        let root_tree = backend
            .read_tree(
                RepoPath::root(),
                &TreeId::from_bytes(root_tree_id.as_bytes()),
            )
            .block_on()?;
        let mut root_entries = root_tree.entries();
        let dir = root_entries.next().unwrap();
        assert_eq!(root_entries.next(), None);
        assert_eq!(dir.name().as_internal_str(), "dir");
        assert_eq!(
            dir.value(),
            &TreeValue::Tree(TreeId::from_bytes(dir_tree_id.as_bytes()))
        );

        let dir_tree = backend
            .read_tree(
                RepoPath::from_internal_string("dir")?,
                &TreeId::from_bytes(dir_tree_id.as_bytes()),
            )
            .block_on()?;
        let mut entries = dir_tree.entries();
        let file = entries.next().unwrap();
        let symlink = entries.next().unwrap();
        assert_eq!(entries.next(), None);
        assert_eq!(file.name().as_internal_str(), "normal");
        assert_eq!(
            file.value(),
            &TreeValue::File {
                id: FileId::from_bytes(blob1.as_bytes()),
                executable: false,
                copy_id: CopyId::placeholder(),
            }
        );
        assert_eq!(symlink.name().as_internal_str(), "symlink");
        assert_eq!(
            symlink.value(),
            &TreeValue::Symlink(SymlinkId::from_bytes(blob2.as_bytes()))
        );

        let commit2 = backend.read_commit(&commit_id2).block_on()?;
        assert_eq!(commit2.parents, vec![commit_id.clone()]);
        assert_eq!(commit.predecessors, vec![]);
        assert_eq!(
            commit.root_tree,
            Merge::resolved(TreeId::from_bytes(root_tree_id.as_bytes()))
        );
        Ok(())
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn read_git_commit_without_importing(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path();
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(&git_repo_path, object_hash);

        let signature = gix::actor::Signature {
            name: GIT_USER.into(),
            email: GIT_EMAIL.into(),
            time: gix::date::Time::now_utc(),
        };
        let empty_tree_id = gix::ObjectId::empty_tree(git_repo.object_hash());
        let git_commit_id = git_repo.commit_as(
            signature.to_ref(&mut TimeBuf::default()),
            signature.to_ref(&mut TimeBuf::default()),
            "refs/heads/main",
            "git commit message",
            empty_tree_id,
            [] as [gix::ObjectId; 0],
        )?;

        let backend = GitBackend::init_external(&settings, store_path, git_repo.path())?;

        // read_commit() without import_head_commits() works as of now. This might be
        // changed later.
        assert!(
            backend
                .read_commit(&CommitId::from_bytes(git_commit_id.as_bytes()))
                .block_on()
                .is_ok()
        );
        assert!(
            backend
                .cached_extra_metadata_table()?
                .get_value(git_commit_id.as_bytes())
                .is_some(),
            "extra metadata should have been be created"
        );
        Ok(())
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn read_signed_git_commit(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path();
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(git_repo_path, object_hash);

        let signature = gix::actor::Signature {
            name: GIT_USER.into(),
            email: GIT_EMAIL.into(),
            time: gix::date::Time::now_utc(),
        };
        let empty_tree_id = gix::ObjectId::empty_tree(git_repo.object_hash());

        let secure_sig =
            "here are some ASCII bytes to be used as a test signature\n\ndefinitely not PGP\n";

        let mut commit = gix::objs::Commit {
            tree: empty_tree_id,
            parents: smallvec::SmallVec::new(),
            author: signature.clone(),
            committer: signature.clone(),
            encoding: None,
            message: "git commit message".into(),
            extra_headers: Vec::new(),
        };

        let mut commit_buf = Vec::new();
        commit.write_to(&mut commit_buf)?;
        let commit_str = str::from_utf8(&commit_buf)?;

        let field = gix::objs::commit::signature_field_name(object_hash);
        commit.extra_headers.push((field.into(), secure_sig.into()));

        let git_commit_id = git_repo.write_object(&commit)?;

        let backend = GitBackend::init_external(&settings, store_path, git_repo.path())?;

        let commit = backend
            .read_commit(&CommitId::from_bytes(git_commit_id.as_bytes()))
            .block_on()?;

        let sig = commit.secure_sig.expect("failed to read the signature");

        // converting to string for nicer assert diff
        assert_eq!(str::from_utf8(&sig.sig)?, secure_sig);
        assert_eq!(str::from_utf8(&sig.data)?, commit_str);
        Ok(())
    }

    #[test]
    fn change_id_parsing() {
        let id = |commit_object_bytes: &[u8]| {
            extract_change_id_from_commit(&ParsedCommit::parse(commit_object_bytes).unwrap())
        };

        let commit_with_id = indoc! {b"
            tree 126799bf8058d1b5c531e93079f4fe79733920dd
            parent bd50783bdf38406dd6143475cd1a3c27938db2ee
            author JJ Fan <jjfan@example.com> 1757112665 -0700
            committer JJ Fan <jjfan@example.com> 1757359886 -0700
            extra-header blah
            change-id lkonztmnvsxytrwkxpvuutrmompwylqq

            test-commit
        "};
        insta::assert_compact_debug_snapshot!(
            id(commit_with_id),
            @r#"Some(ChangeId("efbc06dc4721683f2a45568dbda31e99"))"#
        );

        let commit_without_id = indoc! {b"
            tree 126799bf8058d1b5c531e93079f4fe79733920dd
            parent bd50783bdf38406dd6143475cd1a3c27938db2ee
            author JJ Fan <jjfan@example.com> 1757112665 -0700
            committer JJ Fan <jjfan@example.com> 1757359886 -0700
            extra-header blah

            no id in header
        "};
        insta::assert_compact_debug_snapshot!(
            id(commit_without_id),
            @"None"
        );

        let commit = indoc! {b"
            tree 126799bf8058d1b5c531e93079f4fe79733920dd
            parent bd50783bdf38406dd6143475cd1a3c27938db2ee
            author JJ Fan <jjfan@example.com> 1757112665 -0700
            committer JJ Fan <jjfan@example.com> 1757359886 -0700
            change-id lkonztmnvsxytrwkxpvuutrmompwylqq
            extra-header blah
            change-id abcabcabcabcabcabcabcabcabcabcab

            valid change id first
        "};
        insta::assert_compact_debug_snapshot!(
            id(commit),
            @r#"Some(ChangeId("efbc06dc4721683f2a45568dbda31e99"))"#
        );

        // We only look at the first change id if multiple are present, so this should
        // error
        let commit = indoc! {b"
            tree 126799bf8058d1b5c531e93079f4fe79733920dd
            parent bd50783bdf38406dd6143475cd1a3c27938db2ee
            author JJ Fan <jjfan@example.com> 1757112665 -0700
            committer JJ Fan <jjfan@example.com> 1757359886 -0700
            change-id abcabcabcabcabcabcabcabcabcabcab
            extra-header blah
            change-id lkonztmnvsxytrwkxpvuutrmompwylqq

            valid change id first
        "};
        insta::assert_compact_debug_snapshot!(
            id(commit),
            @"None"
        );
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn round_trip_change_id_via_git_header(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();

        let store_path = temp_dir.path().join("store");
        fs::create_dir(&store_path)?;
        let empty_store_path = temp_dir.path().join("empty_store");
        fs::create_dir(&empty_store_path)?;
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(git_repo_path, object_hash);

        let backend = GitBackend::init_external(&settings, &store_path, git_repo.path())?;
        let original_change_id = ChangeId::from_hex("1111eeee1111eeee1111eeee1111eeee");
        let commit = Commit {
            parents: vec![backend.root_commit_id().clone()],
            predecessors: vec![],
            root_tree: Merge::resolved(backend.empty_tree_id().clone()),
            conflict_labels: Merge::resolved(String::new()),
            change_id: original_change_id.clone(),
            description: "initial".to_string(),
            author: create_signature(),
            committer: create_signature(),
            secure_sig: None,
        };

        let (initial_commit_id, _init_commit) = backend.write_commit(commit, None).block_on()?;
        let commit = backend.read_commit(&initial_commit_id).block_on()?;
        assert_eq!(
            commit.change_id, original_change_id,
            "The change-id header did not roundtrip"
        );

        // Because of how change ids are also persisted in extra proto files,
        // initialize a new store without those files, but reuse the same git
        // storage. This change-id must be derived from the git commit header.
        let no_extra_backend =
            GitBackend::init_external(&settings, &empty_store_path, git_repo.path())?;
        let no_extra_commit = no_extra_backend
            .read_commit(&initial_commit_id)
            .block_on()?;

        assert_eq!(
            no_extra_commit.change_id, original_change_id,
            "The change-id header did not roundtrip"
        );
        Ok(())
    }

    #[test]
    fn read_tree_with_invalid_entry_name() -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path();
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(git_repo_path, gix::hash::Kind::default());

        // Git doesn't create such a tree, but one can be written by hand.
        let blob_id = git_repo.write_blob(b"content")?.detach();
        let tree = gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: gix::object::tree::EntryKind::Blob.into(),
                filename: "a/b".into(),
                oid: blob_id,
            }],
        };
        let tree_id = git_repo.write_object(&tree)?.detach();

        let backend = GitBackend::init_external(&settings, store_path, git_repo.path())?;
        let result = backend
            .read_tree(RepoPath::root(), &TreeId::from_bytes(tree_id.as_bytes()))
            .block_on();
        assert_matches!(result, Err(BackendError::ReadObject { .. }));
        Ok(())
    }

    #[test]
    fn copy_records_with_invalid_path() -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path();
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(git_repo_path, gix::hash::Kind::default());

        // A file renamed to a name ending in "/", which git doesn't create but
        // can be written by hand. The new path, "file/", is not a valid repo
        // path.
        let blob_id = git_repo.write_blob(b"content\n")?.detach();
        let write_tree = |filename: &str| -> TestResult<gix::ObjectId> {
            let tree = gix::objs::Tree {
                entries: vec![gix::objs::tree::Entry {
                    mode: gix::object::tree::EntryKind::Blob.into(),
                    filename: filename.into(),
                    oid: blob_id,
                }],
            };
            Ok(git_repo.write_object(&tree)?.detach())
        };
        let before_tree = write_tree("file")?;
        let after_tree = write_tree("file/")?;
        let signature = gix::actor::Signature {
            name: GIT_USER.into(),
            email: GIT_EMAIL.into(),
            time: gix::date::Time::now_utc(),
        };
        let commit = |reference: &str, tree| {
            git_repo.commit_as(
                signature.to_ref(&mut TimeBuf::default()),
                signature.to_ref(&mut TimeBuf::default()),
                reference,
                "commit",
                tree,
                [] as [gix::ObjectId; 0],
            )
        };
        let before = commit("refs/heads/before", before_tree)?.detach();
        let after = commit("refs/heads/after", after_tree)?.detach();

        let backend = GitBackend::init_external(&settings, store_path, git_repo.path())?;
        let copy_records = |root: gix::ObjectId, head: gix::ObjectId| -> TestResult<Vec<_>> {
            Ok(backend
                .get_copy_records(
                    None,
                    &CommitId::from_bytes(root.as_bytes()),
                    &CommitId::from_bytes(head.as_bytes()),
                )?
                .collect()
                .block_on())
        };
        // The invalid path as the target, then as the source.
        assert_matches!(
            copy_records(before, after)?.as_slice(),
            [Err(BackendError::ReadObject { .. })]
        );
        assert_matches!(
            copy_records(after, before)?.as_slice(),
            [Err(BackendError::ReadObject { .. })]
        );
        Ok(())
    }

    #[test]
    fn read_empty_string_placeholder() {
        let signature1 =
            signature_from_git(b"JJ_EMPTY_STRING <git.author@example.com> 1000 +0100").unwrap();
        assert!(signature1.name.is_empty());
        assert_eq!(signature1.email, "git.author@example.com");
        let signature2 = signature_from_git(b"git committer <JJ_EMPTY_STRING> 2000 -0800").unwrap();
        assert_eq!(signature2.name, "git committer");
        assert!(signature2.email.is_empty());
    }

    #[test]
    fn write_empty_string_placeholder() {
        let signature1 = Signature {
            name: "".to_string(),
            email: "someone@example.com".to_string(),
            timestamp: Timestamp {
                timestamp: MillisSinceEpoch(0),
                tz_offset: 0,
            },
        };
        let git_signature1 = signature_to_git(&signature1, 0).unwrap();
        assert_eq!(
            git_signature1,
            b"JJ_EMPTY_STRING <someone@example.com> 0 +0000"
        );
        let signature2 = Signature {
            name: "Someone".to_string(),
            email: "".to_string(),
            timestamp: Timestamp {
                timestamp: MillisSinceEpoch(0),
                tz_offset: 0,
            },
        };
        let git_signature2 = signature_to_git(&signature2, 0).unwrap();
        assert_eq!(git_signature2, b"Someone <JJ_EMPTY_STRING> 0 +0000");
    }

    /// Test that parents get written correctly
    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn git_commit_parents(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path();
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(&git_repo_path, object_hash);

        let backend = GitBackend::init_external(&settings, store_path, git_repo.path())?;
        let mut commit = Commit {
            parents: vec![],
            predecessors: vec![],
            root_tree: Merge::resolved(backend.empty_tree_id().clone()),
            conflict_labels: Merge::resolved(String::new()),
            change_id: ChangeId::from_hex("abc123"),
            description: "".to_string(),
            author: create_signature(),
            committer: create_signature(),
            secure_sig: None,
        };

        let write_commit = |commit: Commit| -> BackendResult<(CommitId, Commit)> {
            backend.write_commit(commit, None).block_on()
        };

        // No parents
        commit.parents = vec![];
        assert_matches!(
            write_commit(commit.clone()),
            Err(BackendError::Other(err)) if err.to_string().contains("no parents")
        );

        // Only root commit as parent
        commit.parents = vec![backend.root_commit_id().clone()];
        let first_id = write_commit(commit.clone())?.0;
        let first_commit = backend.read_commit(&first_id).block_on()?;
        assert_eq!(first_commit, commit);
        let first_git_commit = git_repo.find_commit(git_id(&first_id))?;
        assert!(first_git_commit.parent_ids().collect_vec().is_empty());

        // Only non-root commit as parent
        commit.parents = vec![first_id.clone()];
        let second_id = write_commit(commit.clone())?.0;
        let second_commit = backend.read_commit(&second_id).block_on()?;
        assert_eq!(second_commit, commit);
        let second_git_commit = git_repo.find_commit(git_id(&second_id))?;
        assert_eq!(
            second_git_commit.parent_ids().collect_vec(),
            vec![git_id(&first_id)]
        );

        // Merge commit
        commit.parents = vec![first_id.clone(), second_id.clone()];
        let merge_id = write_commit(commit.clone())?.0;
        let merge_commit = backend.read_commit(&merge_id).block_on()?;
        assert_eq!(merge_commit, commit);
        let merge_git_commit = git_repo.find_commit(git_id(&merge_id))?;
        assert_eq!(
            merge_git_commit.parent_ids().collect_vec(),
            vec![git_id(&first_id), git_id(&second_id)]
        );

        // Root commit as parent twice
        commit.parents = vec![
            backend.root_commit_id().clone(),
            backend.root_commit_id().clone(),
        ];
        let (root_twice_id, root_twice_commit) = write_commit(commit.clone())?;
        assert_eq!(
            root_twice_commit.parents,
            vec![backend.root_commit_id().clone()]
        );
        assert_eq!(
            backend.read_commit(&root_twice_id).block_on()?,
            root_twice_commit
        );
        let root_twice_git_commit = git_repo.find_commit(git_id(&root_twice_id))?;
        assert!(root_twice_git_commit.parent_ids().collect_vec().is_empty());

        // Merge commit with root as one parent
        commit.parents = vec![first_id.clone(), backend.root_commit_id().clone()];
        let (root_merge_id, root_merge_commit) = write_commit(commit)?;
        assert_eq!(root_merge_commit.parents, vec![first_id.clone()]);
        assert_eq!(
            backend.read_commit(&root_merge_id).block_on()?,
            root_merge_commit
        );
        let root_merge_git_commit = git_repo.find_commit(git_id(&root_merge_id))?;
        assert_eq!(
            root_merge_git_commit.parent_ids().collect_vec(),
            vec![git_id(&first_id)]
        );
        Ok(())
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn write_tree_conflicts(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let store_path = temp_dir.path();
        let git_repo_path = temp_dir.path().join("git");
        let git_repo = git_init(&git_repo_path, object_hash);

        let backend = GitBackend::init_external(&settings, store_path, git_repo.path())?;
        let create_tree = |i| {
            let blob_id = git_repo.write_blob(format!("content {i}")).unwrap();
            let mut tree_builder = git_repo.empty_tree().edit().unwrap();
            tree_builder
                .upsert(
                    format!("file{i}"),
                    gix::object::tree::EntryKind::Blob,
                    blob_id,
                )
                .unwrap();
            TreeId::from_bytes(tree_builder.write().unwrap().as_bytes())
        };

        let root_tree = Merge::from_removes_adds(
            vec![create_tree(0), create_tree(1)],
            vec![create_tree(2), create_tree(3), create_tree(4)],
        );
        let mut commit = Commit {
            parents: vec![backend.root_commit_id().clone()],
            predecessors: vec![],
            root_tree: root_tree.clone(),
            conflict_labels: Merge::resolved(String::new()),
            change_id: ChangeId::from_hex("abc123"),
            description: "".to_string(),
            author: create_signature(),
            committer: create_signature(),
            secure_sig: None,
        };

        let write_commit = |commit: Commit| -> BackendResult<(CommitId, Commit)> {
            backend.write_commit(commit, None).block_on()
        };

        // When writing a tree-level conflict, the root tree on the git side has the
        // individual trees as subtrees.
        let read_commit_id = write_commit(commit.clone())?.0;
        let read_commit = backend.read_commit(&read_commit_id).block_on()?;
        assert_eq!(read_commit, commit);
        let git_commit = git_repo.find_commit(gix::ObjectId::from_bytes_or_panic(
            read_commit_id.as_bytes(),
        ))?;
        let git_tree = git_repo.find_tree(git_commit.tree_id().unwrap())?;
        let jj_conflict_entries = git_tree
            .iter()
            .map(Result::unwrap)
            .filter(|entry| {
                entry.filename().starts_with(b".jjconflict")
                    || entry.filename() == JJ_CONFLICT_README_FILE_NAME
            })
            .collect_vec();
        assert!(
            jj_conflict_entries
                .iter()
                .filter(|entry| entry.filename() != JJ_CONFLICT_README_FILE_NAME)
                .all(|entry| entry.mode().value() == 0o040000)
        );
        let mut iter = jj_conflict_entries.iter();
        let entry = iter.next().unwrap();
        assert_eq!(entry.filename(), b".jjconflict-base-0");
        assert_eq!(
            entry.id().as_bytes(),
            root_tree.get_remove(0).unwrap().as_bytes()
        );
        let entry = iter.next().unwrap();
        assert_eq!(entry.filename(), b".jjconflict-base-1");
        assert_eq!(
            entry.id().as_bytes(),
            root_tree.get_remove(1).unwrap().as_bytes()
        );
        let entry = iter.next().unwrap();
        assert_eq!(entry.filename(), b".jjconflict-side-0");
        assert_eq!(
            entry.id().as_bytes(),
            root_tree.get_add(0).unwrap().as_bytes()
        );
        let entry = iter.next().unwrap();
        assert_eq!(entry.filename(), b".jjconflict-side-1");
        assert_eq!(
            entry.id().as_bytes(),
            root_tree.get_add(1).unwrap().as_bytes()
        );
        let entry = iter.next().unwrap();
        assert_eq!(entry.filename(), b".jjconflict-side-2");
        assert_eq!(
            entry.id().as_bytes(),
            root_tree.get_add(2).unwrap().as_bytes()
        );
        let entry = iter.next().unwrap();
        assert_eq!(entry.filename(), b"JJ-CONFLICT-README");
        assert_eq!(entry.mode().value(), 0o100644);
        assert!(iter.next().is_none());

        // When writing a single tree using the new format, it's represented by a
        // regular git tree.
        commit.root_tree = Merge::resolved(create_tree(5));
        let read_commit_id = write_commit(commit.clone())?.0;
        let read_commit = backend.read_commit(&read_commit_id).block_on()?;
        assert_eq!(read_commit, commit);
        let git_commit = git_repo.find_commit(gix::ObjectId::from_bytes_or_panic(
            read_commit_id.as_bytes(),
        ))?;
        assert_eq!(
            Merge::resolved(TreeId::from_bytes(git_commit.tree_id().unwrap().as_bytes())),
            commit.root_tree
        );
        Ok(())
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn commit_has_ref(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let backend = GitBackend::init_internal(&settings, temp_dir.path(), object_hash)?;
        let git_repo = backend.git_repo();
        let signature = Signature {
            name: "Someone".to_string(),
            email: "someone@example.com".to_string(),
            timestamp: Timestamp {
                timestamp: MillisSinceEpoch(0),
                tz_offset: 0,
            },
        };
        let commit = Commit {
            parents: vec![backend.root_commit_id().clone()],
            predecessors: vec![],
            root_tree: Merge::resolved(backend.empty_tree_id().clone()),
            conflict_labels: Merge::resolved(String::new()),
            change_id: ChangeId::new(vec![42; 16]),
            description: "initial".to_string(),
            author: signature.clone(),
            committer: signature,
            secure_sig: None,
        };
        let commit_id = backend.write_commit(commit, None).block_on()?.0;
        let git_refs = git_repo.references()?;
        let git_ref_ids: Vec<_> = git_refs
            .prefixed("refs/jj/keep/")?
            .map(|x| x.unwrap().id().detach())
            .collect();
        assert!(git_ref_ids.iter().any(|id| *id == git_id(&commit_id)));

        // Concurrently-running GC deletes the ref, leaving the extra metadata.
        for git_ref in git_refs.prefixed("refs/jj/keep/")? {
            git_ref.unwrap().delete().unwrap();
        }
        // Re-imported commit should have new ref.
        backend.import_head_commits([&commit_id])?;
        let git_refs = git_repo.references()?;
        let git_ref_ids: Vec<_> = git_refs
            .prefixed("refs/jj/keep/")?
            .map(|x| x.unwrap().id().detach())
            .collect();
        assert!(git_ref_ids.iter().any(|id| *id == git_id(&commit_id)));
        Ok(())
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn import_head_commits_duplicates(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let backend = GitBackend::init_internal(&settings, temp_dir.path(), object_hash)?;
        let git_repo = backend.git_repo();

        let signature = gix::actor::Signature {
            name: GIT_USER.into(),
            email: GIT_EMAIL.into(),
            time: gix::date::Time::now_utc(),
        };
        let empty_tree_id = gix::ObjectId::empty_tree(git_repo.object_hash());
        let git_commit_id = git_repo
            .commit_as(
                signature.to_ref(&mut TimeBuf::default()),
                signature.to_ref(&mut TimeBuf::default()),
                "refs/heads/main",
                "git commit message",
                empty_tree_id,
                [] as [gix::ObjectId; 0],
            )?
            .detach();
        let commit_id = CommitId::from_bytes(git_commit_id.as_bytes());

        // Ref creation shouldn't fail because of duplicated head ids.
        backend.import_head_commits([&commit_id, &commit_id])?;
        assert!(
            git_repo
                .references()?
                .prefixed("refs/jj/keep/")?
                .any(|git_ref| git_ref.unwrap().id().detach() == git_commit_id)
        );
        Ok(())
    }

    #[test_case(gix::hash::Kind::Sha1 ; "sha1")]
    #[test_case(gix::hash::Kind::Sha256; "sha256")]
    fn overlapping_git_commit_id(object_hash: gix::hash::Kind) -> TestResult {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let backend = GitBackend::init_internal(&settings, temp_dir.path(), object_hash)?;
        let commit1 = Commit {
            parents: vec![backend.root_commit_id().clone()],
            predecessors: vec![],
            root_tree: Merge::resolved(backend.empty_tree_id().clone()),
            conflict_labels: Merge::resolved(String::new()),
            change_id: ChangeId::from_hex("7f0a7ce70354b22efcccf7bf144017c4"),
            description: "initial".to_string(),
            author: create_signature(),
            committer: create_signature(),
            secure_sig: None,
        };

        let write_commit = |commit: Commit| -> BackendResult<(CommitId, Commit)> {
            backend.write_commit(commit, None).block_on()
        };

        let (commit_id1, mut commit2) = write_commit(commit1)?;
        commit2.predecessors.push(commit_id1.clone());
        // `write_commit` should prevent the ids from being the same by changing the
        // committer timestamp of the commit it actually writes.
        let (commit_id2, mut actual_commit2) = write_commit(commit2.clone())?;
        // The returned matches the ID
        assert_eq!(backend.read_commit(&commit_id2).block_on()?, actual_commit2);
        assert_ne!(commit_id2, commit_id1);
        // The committer timestamp should differ
        assert_ne!(
            actual_commit2.committer.timestamp.timestamp,
            commit2.committer.timestamp.timestamp
        );
        // The rest of the commit should be the same
        actual_commit2.committer.timestamp.timestamp = commit2.committer.timestamp.timestamp;
        assert_eq!(actual_commit2, commit2);
        Ok(())
    }

    #[test]
    fn write_signed_commit_sha1() -> TestResult {
        let (obj, sig) = write_signed_commit(gix::hash::Kind::Sha1)?;
        insta::assert_snapshot!(&obj, @"
        tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904
        author Someone <someone@example.com> 0 +0000
        committer Someone <someone@example.com> 0 +0000
        change-id xpxpxpxpxpxpxpxpxpxpxpxpxpxpxpxp
        gpgsig test sig
         hash=03feb0caccbacce2e7b7bca67f4c82292dd487e669ed8a813120c9f82d3fd0801420a1f5d05e1393abfe4e9fc662399ec4a9a1898c5f1e547e0044a52bd4bd29

        initial
        ");
        insta::assert_snapshot!(str::from_utf8(&sig.sig)?, @"
        test sig
        hash=03feb0caccbacce2e7b7bca67f4c82292dd487e669ed8a813120c9f82d3fd0801420a1f5d05e1393abfe4e9fc662399ec4a9a1898c5f1e547e0044a52bd4bd29
        ");
        insta::assert_snapshot!(str::from_utf8(&sig.data)?, @"
        tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904
        author Someone <someone@example.com> 0 +0000
        committer Someone <someone@example.com> 0 +0000
        change-id xpxpxpxpxpxpxpxpxpxpxpxpxpxpxpxp

        initial
        ");
        Ok(())
    }

    #[test]
    fn write_signed_commit_sha256() -> TestResult {
        let (obj, sig) = write_signed_commit(gix::hash::Kind::Sha256)?;
        insta::assert_snapshot!(&obj, @"
        tree 6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321
        author Someone <someone@example.com> 0 +0000
        committer Someone <someone@example.com> 0 +0000
        change-id xpxpxpxpxpxpxpxpxpxpxpxpxpxpxpxp
        gpgsig-sha256 test sig
         hash=d6219e8e5169d409d115848dea4556b3accc76f3cd8dc9b128cc3fe9f71adae275f0e6ce9f98c581a89b960863b61c61b6479cdc20806009d63aecaaa82f4590

        initial
        ");
        insta::assert_snapshot!(str::from_utf8(&sig.sig)?, @"
        test sig
        hash=d6219e8e5169d409d115848dea4556b3accc76f3cd8dc9b128cc3fe9f71adae275f0e6ce9f98c581a89b960863b61c61b6479cdc20806009d63aecaaa82f4590
        ");
        insta::assert_snapshot!(str::from_utf8(&sig.data)?, @"
        tree 6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321
        author Someone <someone@example.com> 0 +0000
        committer Someone <someone@example.com> 0 +0000
        change-id xpxpxpxpxpxpxpxpxpxpxpxpxpxpxpxp

        initial
        ");
        Ok(())
    }

    fn write_signed_commit(object_hash: gix::hash::Kind) -> TestResult<(String, SecureSig)> {
        let settings = user_settings();
        let temp_dir = new_temp_dir();
        let backend = GitBackend::init_internal(&settings, temp_dir.path(), object_hash)?;

        let commit = Commit {
            parents: vec![backend.root_commit_id().clone()],
            predecessors: vec![],
            root_tree: Merge::resolved(backend.empty_tree_id().clone()),
            conflict_labels: Merge::resolved(String::new()),
            change_id: ChangeId::new(vec![42; 16]),
            description: "initial".to_string(),
            author: create_signature(),
            committer: create_signature(),
            secure_sig: None,
        };

        let mut signer = |data: &_| {
            let hash: String = hex_util::encode_hex(&blake2b_hash(data));
            Ok(format!("test sig\nhash={hash}\n").into_bytes())
        };

        let (id, commit) = backend
            .write_commit(commit, Some(&mut signer as &mut SigningFn))
            .block_on()?;
        let returned_sig = commit.secure_sig.expect("failed to return the signature");

        let commit = backend.read_commit(&id).block_on()?;
        let sig = commit.secure_sig.expect("failed to read the signature");
        assert_eq!(&sig, &returned_sig);

        let git_repo = backend.git_repo();
        let obj = git_repo.find_object(gix::ObjectId::from_bytes_or_panic(id.as_bytes()))?;
        Ok((String::from_utf8(obj.data.clone())?, sig))
    }

    fn git_id(commit_id: &CommitId) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(commit_id.as_bytes())
    }

    fn create_signature() -> Signature {
        Signature {
            name: GIT_USER.to_string(),
            email: GIT_EMAIL.to_string(),
            timestamp: Timestamp {
                timestamp: MillisSinceEpoch(0),
                tz_offset: 0,
            },
        }
    }

    // Not using testutils::user_settings() because there is a dependency cycle
    // 'jj_lib (1) -> testutils -> jj_lib (2)' which creates another distinct
    // UserSettings type. testutils returns jj_lib (2)'s UserSettings, whereas
    // our UserSettings type comes from jj_lib (1).
    fn user_settings() -> UserSettings {
        let config = StackedConfig::with_defaults();
        UserSettings::from_config_and_home_dir(config, None).unwrap()
    }
}
