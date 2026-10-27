// Copyright 2026 The Jujutsu Authors
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

//! Fetching from and pushing to Git remotes with grit-lib.

use std::num::NonZeroU32;
use std::path::Path;

use grit_lib::error::RemoteUnreadableReason;
use grit_lib::objects::ObjectId as GitOid;
use grit_lib::progress::NeverCancel;
use grit_lib::progress::ProgressPhase;
use grit_lib::progress::ProgressSink;
use grit_lib::progress::ProgressUnit;
use grit_lib::remote::DefaultHttpClientFactory;
use grit_lib::remote::ListRefsOptions;
use grit_lib::remote::Remote;
use grit_lib::remote::RemoteError;
use grit_lib::remote::RemoteUrl;
use grit_lib::remote::fetch::Progress;
use grit_lib::remote::push_report::PushRefStatus;
use grit_lib::remote::transfer::FetchDepth;
use grit_lib::remote::transfer::FetchOptions;
use grit_lib::remote::transfer::PushOptions;
use grit_lib::remote::transfer::PushRefSpec;
use grit_lib::remote::transfer::TagMode;
use grit_lib::remote::transfer::UpdateMode;
use grit_lib::repo::Repository as GritRepository;
use grit_lib::worktree::AddLinkedWorktreeOptions;
use grit_lib::worktree::PruneWorktreesOptions;
use grit_lib::worktree::WorktreeBranch;
use itertools::Itertools as _;

use crate::git::GitProgress;
use crate::git::GitPushOptions;
use crate::git::GitPushStats;
use crate::git::NegativeRefSpec;
use crate::git::RefSpec;
use crate::git::RefToPush;
use crate::git_subprocess::GitFetchStatus;
use crate::git_subprocess::GitRefUpdates;
use crate::git_subprocess::GitSidebandLineTerminator;
use crate::git_subprocess::GitSubprocessCallback;
use crate::git_subprocess::GitSubprocessError;
use crate::merge::Diff;
use crate::ref_name::GitRefNameBuf;
use crate::ref_name::RefNameBuf;
use crate::ref_name::RemoteName;

/// Converts a grit-lib remote error to the error jj reports.
pub(crate) fn to_transport_error(remote_name: &RemoteName, err: RemoteError) -> GitSubprocessError {
    match err {
        RemoteError::UnknownRemote(_) => {
            GitSubprocessError::NoSuchRepository(remote_name.as_str().to_owned())
        }
        RemoteError::Library(grit_lib::error::Error::RemoteUnreadable {
            url: Some(url),
            reason: RemoteUnreadableReason::RepositoryNotFound,
        }) => GitSubprocessError::NoSuchRepository(url),
        err => GitSubprocessError::Transport(err),
    }
}

/// Loads the remote `remote_name`, with a relative local path resolved from
/// the current directory like the `git` command would.
pub(crate) fn find_remote(
    repo: &GritRepository,
    remote_name: &RemoteName,
    for_push: bool,
) -> Result<Remote, GitSubprocessError> {
    let to_err = |err| to_transport_error(remote_name, err);
    let mut remote = repo.remotes().get(remote_name.as_str()).map_err(to_err)?;
    let url = if for_push {
        remote.push_url()
    } else {
        remote.fetch_url()
    };
    if let RemoteUrl::Local(path) = url
        && path.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        let absolute = cwd.join(path);
        let mut resolved = Remote::from_url(&absolute.to_string_lossy()).map_err(to_err)?;
        resolved.name = remote.name.take();
        resolved.fetch_refspecs = std::mem::take(&mut remote.fetch_refspecs);
        remote = resolved;
    }
    Ok(remote)
}

pub(crate) fn http_client_factory(repo: &GritRepository) -> DefaultHttpClientFactory {
    DefaultHttpClientFactory::with_environment(repo.environment().clone())
}

/// Forwards grit-lib progress and remote messages to a
/// [`GitSubprocessCallback`].
pub(crate) struct CallbackProgress<'a> {
    callback: &'a mut dyn GitSubprocessCallback,
    progress: GitProgress,
    phase: Option<ProgressPhase>,
    /// Bytes of a remote message line not terminated yet.
    pending_message: Vec<u8>,
    /// Whether any remote message was received while the operation ran.
    received_messages: bool,
    /// First error returned by the callback. grit-lib's progress interface
    /// can't fail, so it's reported after the operation.
    error: Option<std::io::Error>,
}

impl<'a> CallbackProgress<'a> {
    pub(crate) fn new(callback: &'a mut dyn GitSubprocessCallback) -> Self {
        Self {
            callback,
            progress: GitProgress::default(),
            phase: None,
            pending_message: Vec::new(),
            received_messages: false,
            error: None,
        }
    }

    fn record<T>(&mut self, result: std::io::Result<T>) {
        if let Err(err) = result
            && self.error.is_none()
        {
            self.error = Some(err);
        }
    }

    fn report_progress(&mut self) {
        if self.callback.needs_progress() {
            let result = self.callback.progress(&self.progress);
            self.record(result);
        }
    }

    fn phase_counter(&mut self) -> Option<&mut (u64, u64)> {
        match self.phase? {
            ProgressPhase::ReceivingObjects => Some(&mut self.progress.objects),
            ProgressPhase::ResolvingDeltas => Some(&mut self.progress.deltas),
            _ => None,
        }
    }

    /// Reports the remote message lines received so far, and returns the
    /// first callback error.
    pub(crate) fn finish(mut self) -> std::io::Result<()> {
        if !self.pending_message.is_empty() {
            let line = std::mem::take(&mut self.pending_message);
            let result = self.callback.remote_sideband(&line, None);
            self.record(result);
        }
        match self.error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Splits remote message bytes into lines, and updates the remote
    /// counting/compressing progress from them like `git` does.
    fn remote_message(&mut self, bytes: &[u8]) {
        self.received_messages |= !bytes.is_empty();
        for &byte in bytes {
            let term = match byte {
                b'\r' => GitSidebandLineTerminator::Cr,
                b'\n' => GitSidebandLineTerminator::Lf,
                _ => {
                    self.pending_message.push(byte);
                    continue;
                }
            };
            let line = std::mem::take(&mut self.pending_message);
            let remote_progress = [
                (&b"Counting objects:"[..], 0),
                (&b"Compressing objects:"[..], 1),
            ]
            .into_iter()
            .find_map(|(prefix, field)| {
                let rest = line.strip_prefix(prefix)?;
                Some((field, parse_progress_counts(rest)?))
            });
            if let Some((field, counts)) = remote_progress {
                match field {
                    0 => self.progress.counted_objects = counts,
                    _ => self.progress.compressed_objects = counts,
                }
                self.report_progress();
            } else {
                let result = self.callback.remote_sideband(&line, Some(term));
                self.record(result);
            }
        }
    }
}

/// Parses `<percent>% (<done>/<total>)` from a progress line.
fn parse_progress_counts(line: &[u8]) -> Option<(u64, u64)> {
    let line = str::from_utf8(line).ok()?;
    let start = line.find('(')?;
    let end = line[start..].find(')')? + start;
    let (done, total) = line[start + 1..end].split_once('/')?;
    Some((done.trim().parse().ok()?, total.trim().parse().ok()?))
}

impl ProgressSink for CallbackProgress<'_> {
    fn start(&mut self, phase: ProgressPhase, total: Option<u64>) {
        self.phase = Some(phase);
        if let Some(counter) = self.phase_counter() {
            *counter = (0, total.unwrap_or(0));
        }
        self.report_progress();
    }

    fn start_with_unit(&mut self, phase: ProgressPhase, unit: ProgressUnit, total: Option<u64>) {
        if unit == ProgressUnit::Bytes {
            // Only object counts are shown.
            self.phase = None;
        } else {
            self.start(phase, total);
        }
    }

    fn inc(&mut self, units: u64) {
        if let Some(counter) = self.phase_counter() {
            counter.0 += units;
            self.report_progress();
        }
    }

    fn set(&mut self, current: u64) {
        if let Some(counter) = self.phase_counter() {
            counter.0 = current;
            self.report_progress();
        }
    }

    fn finish(&mut self) {
        if let Some(counter) = self.phase_counter() {
            counter.0 = counter.1.max(counter.0);
            self.report_progress();
        }
        self.phase = None;
    }
}

impl Progress for CallbackProgress<'_> {
    fn message(&mut self, bytes: &[u8]) {
        self.remote_message(bytes);
    }

    fn remote_sideband_progress(&self) -> bool {
        true
    }
}

/// Fetches `refspecs` from the remote, pruning refs that no longer exist on
/// the remote.
///
/// [`GitFetchStatus::NoRemoteRef`] is returned if the source of a refspec
/// without a pattern doesn't exist on the remote.
pub(crate) fn fetch(
    repo: &GritRepository,
    remote_name: &RemoteName,
    refspecs: &[RefSpec],
    negative_refspecs: &[NegativeRefSpec],
    callback: &mut dyn GitSubprocessCallback,
    depth: Option<NonZeroU32>,
) -> Result<GitFetchStatus, GitSubprocessError> {
    if refspecs.is_empty() {
        return Ok(GitFetchStatus::Updates(GitRefUpdates::default()));
    }
    let to_err = |err| to_transport_error(remote_name, err);
    let mut remote = find_remote(repo, remote_name, false)?;
    // Only the given refspecs are respected, like `git fetch --refmap=`.
    remote.fetch_refspecs.clear();
    let options = FetchOptions {
        refspecs: refspecs.iter().map(RefSpec::to_git_format).collect(),
        negative_refspecs: negative_refspecs
            .iter()
            .map(NegativeRefSpec::to_git_format)
            .collect(),
        // Tags are fetched explicitly by the refspecs.
        tags: TagMode::None,
        prune: true,
        fetch_depth: depth.map(FetchDepth::Depth),
        remote_progress: callback.needs_progress(),
        environment: repo.environment().clone(),
        thread_policy: repo.thread_policy(),
        ..Default::default()
    };
    let http_factory = http_client_factory(repo);
    let mut progress = CallbackProgress::new(callback);
    let result = repo.remotes().fetch(
        &remote,
        options,
        &mut progress,
        &NeverCancel,
        Some(&http_factory),
    );
    progress.finish().map_err(GitSubprocessError::Callback)?;
    let outcome = result.map_err(to_err)?;

    let mut updated = vec![];
    let mut rejected = vec![];
    for update in outcome.updates {
        if update.mode == UpdateMode::SourceObjectNotFound {
            return Ok(GitFetchStatus::NoRemoteRef(update.remote_ref));
        }
        let Some(local_ref) = update.local_ref else {
            continue;
        };
        let null = GitOid::null(repo.odb().hash_algo());
        let oid_diff = Diff::new(
            update.old_oid.unwrap_or(null),
            update.new_oid.unwrap_or(null),
        );
        if update.mode.modified_repo() {
            updated.push((local_ref.into(), oid_diff));
        } else if update.mode.is_rejected() {
            rejected.push((local_ref.into(), oid_diff));
        }
    }
    Ok(GitFetchStatus::Updates(GitRefUpdates { updated, rejected }))
}

/// Pushes `references` to the remote. Every update is forced, but only
/// applied if the remote ref is at the expected location.
pub(crate) fn push(
    repo: &GritRepository,
    remote_name: &RemoteName,
    references: &[RefToPush],
    callback: &mut dyn GitSubprocessCallback,
    options: &GitPushOptions,
) -> Result<GitPushStats, GitSubprocessError> {
    let to_err = |err| to_transport_error(remote_name, err);
    let mut remote = find_remote(repo, remote_name, true)?;
    // An unnamed remote doesn't update remote-tracking refs. jj updates its
    // own refs after the push.
    remote.name = None;
    let refs: Vec<PushRefSpec> = references
        .iter()
        .map(|reference| {
            let refspec = reference.refspec;
            let src = refspec
                .source
                .as_deref()
                .map(|source| {
                    GitOid::from_hex(source).map_err(|_| {
                        GitSubprocessError::External(format!("Invalid object id to push: {source}"))
                    })
                })
                .transpose()?;
            Ok(PushRefSpec {
                src,
                src_ref: None,
                dst: refspec.destination.clone(),
                force: true,
                delete: src.is_none(),
                expected_old: reference.expected_location.copied(),
                expect_absent: reference.expected_location.is_none(),
                hook_src_ref: None,
            })
        })
        .try_collect()?;
    let push_options = PushOptions {
        push_options: options.remote_push_options.clone(),
        // jj doesn't support commit hooks.
        // https://github.com/jj-vcs/jj/issues/3577
        no_verify: true,
        ..Default::default()
    };
    let http_factory = http_client_factory(repo);
    let mut progress = CallbackProgress::new(callback);
    let result = repo.remotes().push(
        &remote,
        &refs,
        push_options,
        &mut progress,
        &NeverCancel,
        Some(&http_factory),
    );
    // Messages from local pushes are only reported in the outcome.
    if let Ok(outcome) = &result
        && !progress.received_messages
    {
        for message in &outcome.remote_messages {
            let mut line = message.clone().into_bytes();
            line.push(b'\n');
            progress.remote_message(&line);
        }
    }
    progress.finish().map_err(GitSubprocessError::Callback)?;
    let outcome = result.map_err(to_err)?;

    let mut push_stats = GitPushStats::default();
    for result in outcome.results {
        let reference: GitRefNameBuf = result.remote_ref.into();
        // Use the reasons `git push --porcelain` reports.
        let reason = match result.status {
            PushRefStatus::Ok | PushRefStatus::UpToDate => {
                push_stats.pushed.push(reference);
                continue;
            }
            PushRefStatus::RemoteRejected => {
                push_stats.remote_rejected.push((reference, result.message));
                continue;
            }
            PushRefStatus::RejectStale => "stale info",
            PushRefStatus::RejectNonFastForward => "non-fast-forward",
            PushRefStatus::RejectAlreadyExists => "already exists",
            PushRefStatus::RejectFetchFirst => "fetch first",
            PushRefStatus::RejectNeedsForce => "needs force",
            PushRefStatus::RejectRemoteRefMissing => "remote ref does not exist",
            PushRefStatus::AtomicPushFailed => "atomic push failed",
        };
        push_stats
            .rejected
            .push((reference, Some(reason.to_owned())));
    }
    Ok(push_stats)
}

/// Queries the remote for its default branch, the branch its HEAD points to.
pub(crate) fn default_branch(
    repo: &GritRepository,
    remote_name: &RemoteName,
) -> Result<Option<RefNameBuf>, GitSubprocessError> {
    let remote = find_remote(repo, remote_name, false)?;
    let options = ListRefsOptions {
        symrefs: true,
        ..Default::default()
    };
    let http_factory = http_client_factory(repo);
    let refs = repo
        .remotes()
        .ls_remote(&remote, &options, Some(&http_factory))
        .map_err(|err| to_transport_error(remote_name, err))?;
    // An unborn HEAD (listed with the null id) has no default branch, like
    // `git remote show` reports.
    let Some(head) = refs
        .iter()
        .find(|remote_ref| remote_ref.name == "HEAD" && !remote_ref.oid.is_zero())
    else {
        return Ok(None);
    };
    let default_branch = match head.symref_target.as_deref() {
        Some(target) => target.strip_prefix("refs/heads/").map(RefNameBuf::from),
        None => guess_detached_head_branch(repo, &refs, &head.oid),
    };
    Ok(default_branch)
}

/// Guesses the branch a detached remote HEAD stands for, like Git's
/// `guess_remote_head()`: the default branch name if it points to the same
/// commit, otherwise the first such branch.
fn guess_detached_head_branch(
    repo: &GritRepository,
    refs: &[grit_lib::remote::RemoteRef],
    head_oid: &GitOid,
) -> Option<RefNameBuf> {
    let default_name = repo
        .config()
        .ok()
        .and_then(|config| config.get("init.defaultBranch"))
        .unwrap_or_else(|| "master".to_owned());
    let matching_branches = || {
        refs.iter()
            .filter(|remote_ref| remote_ref.oid == *head_oid)
            .filter_map(|remote_ref| remote_ref.name.strip_prefix("refs/heads/"))
    };
    let branch = matching_branches()
        .find(|name| *name == default_name)
        .or_else(|| matching_branches().next())?;
    Some(branch.into())
}

/// Creates a Git worktree at `destination` on the unborn branch
/// `branch_name`, without checking anything out.
pub(crate) fn add_orphan_worktree(
    repo: &GritRepository,
    destination: &Path,
    branch_name: &str,
) -> Result<(), GitSubprocessError> {
    let options = AddLinkedWorktreeOptions {
        path: destination.to_owned(),
        branch: Some(WorktreeBranch::Orphan(branch_name.to_owned())),
        start_point: None,
        force: false,
        lock: None,
        no_checkout: true,
        track: false,
    };
    repo.worktrees()
        .add(&options)
        .map_err(GitSubprocessError::Worktree)?;
    Ok(())
}

/// Fixes up the paths Git recorded for the worktree at `worktree_path`, after
/// the worktree or the repository has been moved.
pub(crate) fn repair_worktree(
    repo: &GritRepository,
    worktree_path: &Path,
) -> Result<(), GitSubprocessError> {
    repo.worktrees()
        .repair(&[worktree_path], false)
        .map_err(GitSubprocessError::Worktree)?;
    Ok(())
}

/// Removes the bookkeeping of worktrees whose directories are gone.
pub(crate) fn prune_worktrees(repo: &GritRepository) -> Result<(), GitSubprocessError> {
    let options = PruneWorktreesOptions { dry_run: false };
    grit_lib::worktree::prune_linked_worktrees(repo, &options)
        .map_err(GitSubprocessError::Worktree)?;
    Ok(())
}
