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

//! Helpers for setting up and inspecting Git repositories in tests.

use std::ffi::OsStr;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use bstr::BStr;
use bstr::BString;
pub use grit_lib::objects::HashAlgo;
pub use grit_lib::objects::ObjectId;
pub use grit_lib::objects::ObjectKind;
pub use grit_lib::objects::TreeEntry;
use grit_lib::prelude::Environment;
use grit_lib::prelude::RepositoryOptions;
use grit_lib::refs::store::Expected;
use grit_lib::refs::store::RawRef;
use grit_lib::refs::store::RefTransaction;
use grit_lib::refs::store::RefUpdate;
use grit_lib::refs::store::RefUpdateFlags;
use grit_lib::refs::store::ReflogUpdate;

pub const GIT_USER: &str = "Someone";
pub const GIT_EMAIL: &str = "someone@example.org";

/// Error type of the helpers.
pub type Error = eyre::Report;

/// An environment that doesn't read the user's or system's Git config.
fn isolated_environment() -> Environment {
    let mut env = Environment::empty();
    env.cwd = std::env::current_dir().unwrap_or_default();
    env.git_config_nosystem = Some("1".to_owned());
    env.git_config_pairs = vec![
        ("user.name".to_owned(), GIT_USER.to_owned()),
        ("user.email".to_owned(), GIT_EMAIL.to_owned()),
        ("init.defaultBranch".to_owned(), "master".to_owned()),
    ];
    env
}

fn open_options() -> RepositoryOptions {
    RepositoryOptions::with_environment(isolated_environment())
}

/// A Git repository opened for tests, with a few helpers in the style of the
/// `git` command.
#[derive(Debug)]
pub struct Repository {
    inner: grit_lib::repo::Repository,
}

impl From<grit_lib::repo::Repository> for Repository {
    fn from(inner: grit_lib::repo::Repository) -> Self {
        Self { inner }
    }
}

impl Deref for Repository {
    type Target = grit_lib::repo::Repository;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Opens the repository at `directory`, which is either a work tree or a Git
/// directory.
pub fn open(directory: impl Into<PathBuf>) -> Repository {
    let directory = directory.into();
    let dot_git = directory.join(".git");
    let inner = if dot_git.exists() {
        let git_dir = grit_lib::repo::resolve_dot_git(&dot_git).unwrap();
        grit_lib::repo::Repository::open(&git_dir, &open_options().with_work_tree(&directory))
    } else {
        grit_lib::repo::Repository::open(&directory, &open_options())
    }
    .unwrap();
    Repository { inner }
}

fn init_with(directory: &Path, bare: bool, hash_algo: HashAlgo) -> Repository {
    let options = grit_lib::repo::InitOptions::new()
        .with_bare(bare)
        .with_initial_branch("master")
        .with_object_format(hash_algo)
        .with_environment(isolated_environment());
    grit_lib::repo::Repository::init(directory, &options).unwrap();
    open(directory)
}

pub fn init(directory: impl AsRef<Path>) -> Repository {
    init_with(directory.as_ref(), false, HashAlgo::Sha1)
}

pub fn init_with_hash(directory: impl AsRef<Path>, hash_algo: HashAlgo) -> Repository {
    init_with(directory.as_ref(), false, hash_algo)
}

pub fn init_bare(directory: impl AsRef<Path>) -> Repository {
    init_with(directory.as_ref(), true, HashAlgo::Sha1)
}

pub fn clone(dest_path: &Path, repo_url: &str, remote_name: Option<&str>) -> Repository {
    let remote_name = remote_name.unwrap_or("origin");
    // The code to clone a repo is non-trivial, so just spawn a git subprocess.
    let output = Command::new("git")
        .args(["clone", repo_url, "--origin", remote_name])
        .arg(dest_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git cloning failed with {}:\n{}\n----- stderr -----\n{}",
        output.status,
        BString::from(output.stdout),
        BString::from(output.stderr),
    );

    open(dest_path)
}

/// Writes out gitlink entry pointing to the `target_repo`.
pub fn create_gitlink(src_repo: impl AsRef<Path>, target_repo: impl AsRef<Path>) {
    let git_link_path = src_repo.as_ref().join(".git");
    std::fs::write(
        git_link_path,
        format!("gitdir: {}\n", target_repo.as_ref().display()),
    )
    .unwrap();
}

pub fn remove_config_value(repo: Repository, section: &str, key: &str) {
    let path = grit_lib::repo::local_config_write_path(repo.git_dir());
    let mut config =
        grit_lib::config::ConfigFile::from_path(&path, grit_lib::config::ConfigScope::Local)
            .unwrap()
            .unwrap();
    config.unset(&format!("{section}.{key}")).unwrap();
    config.write().unwrap();
}

/// Path to the repository-local config file.
pub fn config_path(repo: &Repository) -> PathBuf {
    grit_lib::repo::local_config_write_path(repo.git_dir())
}

/// Replaces all values of `key` in the repository-local config file.
pub fn set_config_values(repo: &Repository, key: &str, values: &[&str]) {
    let path = config_path(repo);
    let mut config =
        grit_lib::config::ConfigFile::from_path(&path, grit_lib::config::ConfigScope::Local)
            .unwrap()
            .unwrap();
    config.unset(key).unwrap();
    for value in values {
        config.add_value(key, value).unwrap();
    }
    config.write().unwrap();
}

/// Expected state of a ref before it's updated.
#[derive(Clone, Debug)]
pub enum PreviousValue {
    Any,
    MustExist,
    MustNotExist,
    MustExistAndMatch(ObjectId),
}

impl From<PreviousValue> for Expected {
    fn from(value: PreviousValue) -> Self {
        match value {
            PreviousValue::Any => Self::Any,
            PreviousValue::MustExist => Self::Exists,
            PreviousValue::MustNotExist => Self::Missing,
            PreviousValue::MustExistAndMatch(oid) => Self::Oid(oid),
        }
    }
}

/// The value of a ref.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target(RawRef);

impl Target {
    /// The object id of a direct ref. Panics for a symbolic ref.
    pub fn id(&self) -> ObjectId {
        self.try_id().expect("ref should be direct")
    }

    pub fn try_id(&self) -> Option<ObjectId> {
        match &self.0 {
            RawRef::Direct(oid) => Some(*oid),
            RawRef::Symbolic(_) => None,
        }
    }

    /// The target name of a symbolic ref.
    pub fn try_name(&self) -> Option<&BStr> {
        match &self.0 {
            RawRef::Direct(_) => None,
            RawRef::Symbolic(name) => Some(name.as_str().into()),
        }
    }
}

/// A ref read from a [`Repository`].
#[derive(Debug)]
pub struct Reference<'repo> {
    repo: &'repo Repository,
    name: String,
    target: RawRef,
}

impl Reference<'_> {
    pub fn name(&self) -> &BStr {
        self.name.as_str().into()
    }

    pub fn target(&self) -> Target {
        Target(self.target.clone())
    }

    /// The object the ref resolves to, following symbolic refs.
    pub fn id(&self) -> ObjectId {
        match &self.target {
            RawRef::Direct(oid) => *oid,
            RawRef::Symbolic(name) => self.repo.refs().resolve(name).unwrap(),
        }
    }

    /// The object the ref resolves to, with annotated tags peeled.
    pub fn peel_to_id(&self) -> Result<ObjectId, Error> {
        let mut oid = self.id();
        loop {
            let object = self.repo.odb().read(&oid)?;
            if object.kind != ObjectKind::Tag {
                return Ok(oid);
            }
            oid = grit_lib::objects::tag_object_line_oid(&object.data)
                .ok_or_else(|| eyre::eyre!("invalid tag object {oid}"))?;
        }
    }

    pub fn into_fully_peeled_id(self) -> Result<ObjectId, Error> {
        self.peel_to_id()
    }

    /// The commit the ref resolves to, with annotated tags peeled.
    pub fn peel_to_commit(&self) -> Result<Commit, Error> {
        let id = self.peel_to_id()?;
        let data = self.repo.objects().read_commit(&id)?;
        Ok(Commit { id, data })
    }

    pub fn delete(self) -> Result<(), Error> {
        let expected = match &self.target {
            RawRef::Direct(oid) => Expected::Oid(*oid),
            RawRef::Symbolic(name) => Expected::Symref(name.clone()),
        };
        self.repo.edit_ref(&self.name, None, expected, None)?;
        self.repo.refs().delete_reflog(&self.name)?;
        Ok(())
    }
}

/// A commit read from a [`Repository`].
#[derive(Debug)]
pub struct Commit {
    id: ObjectId,
    data: grit_lib::objects::CommitData,
}

impl Commit {
    pub fn id(&self) -> ObjectId {
        self.id
    }

    pub fn tree_id(&self) -> ObjectId {
        self.data.tree
    }

    pub fn parent_ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.data.parents.iter().copied()
    }

    pub fn message(&self) -> &str {
        &self.data.message
    }
}

/// A tree id.
#[derive(Debug)]
pub struct Tree {
    id: ObjectId,
}

impl Tree {
    pub fn id(&self) -> ObjectId {
        self.id
    }
}

/// A full ref name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FullName(BString);

impl FullName {
    pub fn as_bstr(&self) -> &BStr {
        self.0.as_ref()
    }
}

/// The state of HEAD.
#[derive(Debug)]
pub struct Head {
    target: Option<RawRef>,
    unborn: bool,
}

impl Head {
    pub fn is_detached(&self) -> bool {
        matches!(self.target, Some(RawRef::Direct(_)))
    }

    pub fn is_unborn(&self) -> bool {
        self.unborn
    }

    /// The branch HEAD points to, if it's symbolic.
    pub fn name(&self) -> Option<&BStr> {
        match &self.target {
            Some(RawRef::Symbolic(name)) => Some(name.as_str().into()),
            _ => None,
        }
    }
}

impl Repository {
    /// Path to the Git directory.
    pub fn path(&self) -> &Path {
        self.inner.git_dir()
    }

    pub fn workdir(&self) -> Option<&Path> {
        self.inner.work_tree()
    }

    pub fn object_hash(&self) -> HashAlgo {
        self.odb().hash_algo()
    }

    /// Finds a ref by full or short name, trying Git's rev-parse rules.
    pub fn try_find_reference(&self, name: &str) -> Result<Option<Reference<'_>>, Error> {
        let candidates = if name == "HEAD" || name.starts_with("refs/") {
            vec![name.to_owned()]
        } else {
            vec![
                name.to_owned(),
                format!("refs/{name}"),
                format!("refs/tags/{name}"),
                format!("refs/heads/{name}"),
                format!("refs/remotes/{name}"),
                format!("refs/remotes/{name}/HEAD"),
            ]
        };
        for candidate in candidates {
            if let Some(target) = self.refs().read_raw(&candidate)? {
                return Ok(Some(Reference {
                    repo: self,
                    name: candidate,
                    target,
                }));
            }
        }
        Ok(None)
    }

    /// The refs whose names start with `prefix`, sorted by name.
    pub fn list_references(&self, prefix: &str) -> Result<Vec<Reference<'_>>, Error> {
        let mut refs = vec![];
        self.refs().for_each_ref(prefix, &mut |entry| {
            refs.push(Reference {
                repo: self,
                name: entry.name.clone(),
                target: entry.value.clone(),
            });
            std::ops::ControlFlow::Continue(())
        })?;
        Ok(refs)
    }

    pub fn find_reference(&self, name: &str) -> Result<Reference<'_>, Error> {
        self.try_find_reference(name)?
            .ok_or_else(|| eyre::eyre!("reference {name} not found"))
    }

    fn edit_ref(
        &self,
        name: &str,
        new_value: Option<RawRef>,
        expected: Expected,
        message: Option<&str>,
    ) -> Result<(), Error> {
        let reflog = message.map(|message| ReflogUpdate {
            identity: format!("{GIT_USER} <{GIT_EMAIL}>"),
            message: message.to_owned(),
            time: self.identity_now(),
            old_oid: None,
            new_oid: None,
        });
        let update = RefUpdate {
            name: name.to_owned(),
            new_value,
            expected,
            reflog,
            flags: RefUpdateFlags {
                no_deref: true,
                log_only: false,
                ..RefUpdateFlags::default()
            },
        };
        let transaction = RefTransaction::new().update(update)?;
        self.refs().prepare(transaction)?.commit()?;
        Ok(())
    }

    /// Creates or updates the ref `name` to point to `target`.
    pub fn reference(
        &self,
        name: impl AsRef<str>,
        target: ObjectId,
        constraint: PreviousValue,
        message: &str,
    ) -> Result<Reference<'_>, Error> {
        let name = name.as_ref();
        let log = grit_lib::refs::should_autocreate_reflog(self.git_dir(), name)
            || self.refs().reflog_exists(name)?;
        self.edit_ref(
            name,
            Some(RawRef::Direct(target)),
            constraint.into(),
            log.then_some(message),
        )?;
        self.find_reference(name)
    }

    /// Creates the lightweight tag `refs/tags/<name>`.
    pub fn tag_reference(
        &self,
        name: &str,
        target: ObjectId,
        constraint: PreviousValue,
    ) -> Result<Reference<'_>, Error> {
        self.reference(&format!("refs/tags/{name}"), target, constraint, "")
    }

    /// Creates an annotated tag object for `target` and the tag ref pointing
    /// to it.
    pub fn tag(
        &self,
        name: &str,
        target: ObjectId,
        target_kind: ObjectKind,
        tagger: Option<&str>,
        message: &str,
        constraint: PreviousValue,
    ) -> Result<Reference<'_>, Error> {
        let tag = grit_lib::objects::TagData {
            object: target,
            object_type: target_kind.as_str().to_owned(),
            tag: name.to_owned(),
            tagger: tagger.map(ToOwned::to_owned),
            extra_headers: vec![],
            message: message.to_owned(),
        };
        let data = grit_lib::objects::serialize_tag(&tag);
        let tag_oid = self.odb().write(ObjectKind::Tag, &data)?;
        self.tag_reference(name, tag_oid, constraint)
    }

    pub fn head(&self) -> Result<Head, Error> {
        let target = self.refs().read_raw("HEAD")?;
        let unborn = self.refs().resolve("HEAD").is_err();
        Ok(Head { target, unborn })
    }

    /// The commit HEAD points to.
    pub fn head_id(&self) -> Result<ObjectId, Error> {
        Ok(self.refs().resolve("HEAD")?)
    }

    /// The branch HEAD points to, if it's symbolic.
    pub fn head_name(&self) -> Result<Option<FullName>, Error> {
        Ok(self.head()?.name().map(|name| FullName(name.to_owned())))
    }

    pub fn find_commit(&self, id: ObjectId) -> Result<Commit, Error> {
        let data = self.objects().read_commit(&id)?;
        Ok(Commit { id, data })
    }

    /// Reads the entries of the tree `id`.
    pub fn find_tree(&self, id: ObjectId) -> Result<Vec<TreeEntry>, Error> {
        let data = self.odb().read(&id)?.data;
        Ok(grit_lib::objects::parse_tree_with_oid_len(
            &data,
            self.object_hash().len(),
        )?)
    }

    /// Writes a tree with the given entries as is, without validating or
    /// sorting them.
    pub fn write_raw_tree(&self, entries: &[TreeEntry]) -> Result<ObjectId, Error> {
        let data = grit_lib::objects::serialize_tree(entries);
        Ok(self.odb().write(ObjectKind::Tree, &data)?)
    }

    pub fn empty_tree(&self) -> Tree {
        Tree {
            id: self.empty_tree_id(),
        }
    }

    /// The linked worktrees of the repository.
    pub fn worktrees(&self) -> Result<Vec<grit_lib::worktree::WorktreeEntry>, Error> {
        let common_dir = std::fs::canonicalize(self.common_git_dir())?;
        Ok(self
            .inner
            .worktrees()
            .list()?
            .into_iter()
            .filter(|entry| {
                std::fs::canonicalize(&entry.admin_dir).ok().as_ref() != Some(&common_dir)
            })
            .collect())
    }

    pub fn write_blob(&self, data: impl AsRef<[u8]>) -> Result<ObjectId, Error> {
        Ok(self.odb().write(ObjectKind::Blob, data.as_ref())?)
    }

    pub fn write_tree(&self, entries: Vec<TreeEntry>) -> Result<ObjectId, Error> {
        Ok(self.objects().write_tree(entries)?)
    }

    pub fn empty_tree_id(&self) -> ObjectId {
        self.odb().empty_tree_oid().unwrap()
    }

    /// Reads the tree of the commit `commit_id`.
    pub fn commit_tree_id(&self, commit_id: ObjectId) -> Result<ObjectId, Error> {
        Ok(self.objects().commit_tree(&commit_id)?)
    }
}

pub struct CommitResult {
    pub tree_id: ObjectId,
    pub commit_id: ObjectId,
}

/// Returns `tree_id` with `filename` (which may contain slashes) set to the
/// blob `blob_id`.
fn upsert_tree_entry(
    repo: &Repository,
    tree_id: Option<ObjectId>,
    filename: &str,
    blob_id: ObjectId,
) -> ObjectId {
    let mut entries = match tree_id {
        Some(tree_id) => {
            let data = repo.odb().read(&tree_id).unwrap().data;
            grit_lib::objects::parse_tree_with_oid_len(&data, repo.object_hash().len()).unwrap()
        }
        None => vec![],
    };
    let (name, rest) = match filename.split_once('/') {
        Some((dir, rest)) => (dir, Some(rest)),
        None => (filename, None),
    };
    let existing = entries
        .iter()
        .position(|entry| entry.name == name.as_bytes());
    let (mode, oid) = match rest {
        Some(rest) => {
            let subtree = existing
                .map(|i| &entries[i])
                .filter(|entry| entry.mode == 0o040000)
                .map(|entry| entry.oid);
            (0o040000, upsert_tree_entry(repo, subtree, rest, blob_id))
        }
        None => (0o100644, blob_id),
    };
    let entry = TreeEntry {
        mode,
        name: name.as_bytes().to_vec(),
        oid,
    };
    match existing {
        Some(i) => entries[i] = entry,
        None => entries.push(entry),
    }
    repo.write_tree(entries).unwrap()
}

pub fn add_commit(
    repo: &Repository,
    reference: &str,
    filename: &str,
    content: &[u8],
    message: &str,
    parents: &[ObjectId],
) -> CommitResult {
    let blob_oid = repo.write_blob(content).unwrap();
    let parent_tree = parents
        .first()
        .map(|commit_id| repo.commit_tree_id(*commit_id).unwrap());
    let tree_id = upsert_tree_entry(repo, parent_tree, filename, blob_oid);
    let commit_id = write_commit(repo, reference, tree_id, message, parents);
    CommitResult { tree_id, commit_id }
}

pub fn write_commit(
    repo: &Repository,
    reference: &str,
    tree_id: ObjectId,
    message: &str,
    parents: &[ObjectId],
) -> ObjectId {
    let signature = signature(GIT_USER, GIT_EMAIL, 0, 0);
    let data = commit_bytes(&signature, &signature, tree_id, parents, &[], message);
    let commit_id = repo.odb().write(ObjectKind::Commit, &data).unwrap();
    // Like `git commit`, update the branch HEAD points to.
    let reference = if reference == "HEAD" {
        match repo.refs().read_raw("HEAD").unwrap() {
            Some(RawRef::Symbolic(name)) => name,
            _ => "HEAD".to_owned(),
        }
    } else {
        reference.to_owned()
    };
    let first_line = message.lines().next().unwrap_or("");
    let log_message = if parents.is_empty() {
        format!("commit (initial): {first_line}")
    } else {
        format!("commit: {first_line}")
    };
    repo.reference(&reference, commit_id, PreviousValue::Any, &log_message)
        .unwrap();
    commit_id
}

/// Formats a signature as `name <email> seconds +hhmm`.
pub fn signature(name: &str, email: &str, seconds: i64, offset_minutes: i32) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let offset = offset_minutes.unsigned_abs();
    format!(
        "{name} <{email}> {seconds} {sign}{:02}{:02}",
        offset / 60,
        offset % 60
    )
}

/// The signature of the test user at the current time.
pub fn signature_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    signature(GIT_USER, GIT_EMAIL, now.as_secs() as i64, 0)
}

/// Serializes a commit object in Git's format.
pub fn commit_bytes(
    author: &str,
    committer: &str,
    tree_id: ObjectId,
    parents: &[ObjectId],
    extra_headers: &[(&str, &str)],
    message: &str,
) -> Vec<u8> {
    let mut data = format!("tree {tree_id}\n");
    for parent in parents {
        data.push_str(&format!("parent {parent}\n"));
    }
    data.push_str(&format!("author {author}\ncommitter {committer}\n"));
    for (name, value) in extra_headers {
        let value = value.strip_suffix('\n').unwrap_or(value);
        data.push_str(&format!("{name} {}\n", value.replace('\n', "\n ")));
    }
    data.push('\n');
    data.push_str(message);
    data.into_bytes()
}

/// Writes a commit with the given signatures, and points `reference` to it.
pub fn write_commit_as(
    repo: &Repository,
    reference: Option<&str>,
    author: &str,
    committer: &str,
    tree_id: ObjectId,
    parents: &[ObjectId],
    message: &str,
) -> ObjectId {
    let data = commit_bytes(author, committer, tree_id, parents, &[], message);
    let commit_id = repo.odb().write(ObjectKind::Commit, &data).unwrap();
    if let Some(reference) = reference {
        repo.reference(reference, commit_id, PreviousValue::Any, "commit")
            .unwrap();
    }
    commit_id
}

pub fn set_head_to_id(repo: &Repository, target: ObjectId) {
    repo.edit_ref("HEAD", Some(RawRef::Direct(target)), Expected::Any, None)
        .unwrap();
}

pub fn set_symbolic_reference(repo: &Repository, reference: &str, target: &str) {
    repo.edit_ref(
        reference,
        Some(RawRef::Symbolic(target.to_owned())),
        Expected::Any,
        Some("create symbolic reference"),
    )
    .unwrap();
}

/// Writes the files of `tree_id` to the work tree, and sets the index to the
/// tree.
pub fn checkout_tree_index(repo: &Repository, tree_id: ObjectId) {
    fn checkout(
        repo: &Repository,
        tree_id: ObjectId,
        prefix: &str,
        index: &mut grit_lib::index::Index,
    ) {
        let workdir = repo.workdir().unwrap();
        let data = repo.odb().read(&tree_id).unwrap().data;
        let entries =
            grit_lib::objects::parse_tree_with_oid_len(&data, repo.object_hash().len()).unwrap();
        for entry in entries {
            let name = str::from_utf8(&entry.name).unwrap();
            let rel_path = format!("{prefix}{name}");
            let path = workdir.join(&rel_path);
            match entry.mode {
                0o040000 => {
                    std::fs::create_dir_all(&path).unwrap();
                    checkout(repo, entry.oid, &format!("{rel_path}/"), index);
                    continue;
                }
                0o160000 => {
                    std::fs::create_dir_all(&path).unwrap();
                }
                0o120000 => {
                    let target = repo.odb().read(&entry.oid).unwrap().data;
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(str::from_utf8(&target).unwrap(), &path).unwrap();
                    #[cfg(windows)]
                    std::fs::write(&path, target).unwrap();
                }
                mode => {
                    let content = repo.odb().read(&entry.oid).unwrap().data;
                    std::fs::write(&path, content).unwrap();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        let perm = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(perm))
                            .unwrap();
                    }
                }
            }
            let index_entry =
                grit_lib::index::entry_from_stat(&path, rel_path.as_bytes(), entry.oid, entry.mode)
                    .unwrap();
            index.entries_mut().push(index_entry);
        }
    }

    let mut index = grit_lib::index::Index::new();
    index.hash_algo = repo.object_hash();
    checkout(repo, tree_id, "", &mut index);
    index.mark_entries_unsorted();
    index.sort();
    repo.index_handle().write(&mut index).unwrap();
}

/// Names an index stage the way tests print it.
pub fn index_stage_name(stage: u8) -> &'static str {
    match stage {
        0 => "Unconflicted",
        1 => "Base",
        2 => "Ours",
        _ => "Theirs",
    }
}

/// Names an index entry mode the way tests print it.
pub fn index_mode_name(mode: u32) -> &'static str {
    match mode {
        0o100644 => "Mode(FILE)",
        0o100755 => "Mode(FILE_EXECUTABLE)",
        0o120000 => "Mode(SYMLINK)",
        0o160000 => "Mode(COMMIT)",
        _ => "Mode(DIR)",
    }
}

/// Writes the files in the index to the work tree.
pub fn checkout_index(repo: &Repository) {
    let workdir = repo.workdir().unwrap();
    let index = repo.index_handle().load().unwrap();
    for entry in index.entries().iter().filter(|entry| entry.stage() == 0) {
        let path = workdir.join(str::from_utf8(&entry.path).unwrap());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let content = repo.odb().read(&entry.oid).unwrap().data;
        std::fs::write(&path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let perm = if entry.mode & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(perm)).unwrap();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum GitStatusInfo {
    Index(IndexStatus),
    Worktree(WorktreeStatus),
}

#[derive(Debug, PartialEq, Eq)]
pub enum IndexStatus {
    Addition,
    Deletion,
    Rename,
    Modification,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorktreeStatus {
    Removed,
    Added,
    Modified,
    TypeChange,
    Renamed,
    Copied,
    IntentToAdd,
    Conflict,
    Ignored,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GitStatus {
    path: String,
    status: GitStatusInfo,
}

pub fn status(repo: &Repository) -> Vec<GitStatus> {
    use grit_lib::diff::DiffStatus;
    use grit_lib::porcelain::status::IgnoredMode;
    use grit_lib::porcelain::status::StatusOptions;
    use grit_lib::porcelain::status::UntrackedMode;

    let options = StatusOptions {
        untracked: UntrackedMode::All,
        ignored: IgnoredMode::Matching,
        ..Default::default()
    };
    let model = grit_lib::porcelain::status::status(
        repo,
        &options,
        &mut grit_lib::progress::NullProgress,
        &grit_lib::progress::NeverCancel,
    )
    .unwrap();

    let intent_to_add: Vec<&[u8]> = model
        .index
        .entries()
        .iter()
        .filter(|entry| entry.intent_to_add())
        .map(|entry| entry.path.as_slice())
        .collect();
    let mut status = vec![];
    for entry in &model.staged {
        let index_status = match entry.status {
            DiffStatus::Added => IndexStatus::Addition,
            DiffStatus::Deleted => IndexStatus::Deletion,
            DiffStatus::Renamed | DiffStatus::Copied => IndexStatus::Rename,
            _ => IndexStatus::Modification,
        };
        if index_status == IndexStatus::Addition && intent_to_add.contains(&entry.path().as_bytes())
        {
            continue;
        }
        status.push(GitStatus {
            path: entry.path().to_owned(),
            status: GitStatusInfo::Index(index_status),
        });
    }
    for entry in &model.unstaged {
        let worktree_status = if intent_to_add.contains(&entry.path().as_bytes()) {
            WorktreeStatus::IntentToAdd
        } else {
            match entry.status {
                DiffStatus::Added => WorktreeStatus::Added,
                DiffStatus::Deleted => WorktreeStatus::Removed,
                DiffStatus::Renamed => WorktreeStatus::Renamed,
                DiffStatus::Copied => WorktreeStatus::Copied,
                DiffStatus::TypeChanged => WorktreeStatus::TypeChange,
                DiffStatus::Unmerged => WorktreeStatus::Conflict,
                _ => WorktreeStatus::Modified,
            }
        };
        status.push(GitStatus {
            path: entry.path().to_owned(),
            status: GitStatusInfo::Worktree(worktree_status),
        });
    }
    // Directories are reported without the trailing slash.
    let path_name = |path: &String| path.strip_suffix('/').unwrap_or(path).to_owned();
    for path in &model.untracked {
        status.push(GitStatus {
            path: path_name(path),
            status: GitStatusInfo::Worktree(WorktreeStatus::Added),
        });
    }
    for path in &model.ignored {
        status.push(GitStatus {
            path: path_name(path),
            status: GitStatusInfo::Worktree(WorktreeStatus::Ignored),
        });
    }

    status.sort_by(|a, b| a.path.cmp(&b.path));
    status
}

pub struct IndexManager<'a> {
    index: grit_lib::index::Index,
    repo: &'a Repository,
}

impl<'a> IndexManager<'a> {
    pub fn new(repo: &'a Repository) -> Self {
        let index = repo.index_handle().load().unwrap();
        Self { index, repo }
    }

    pub fn add_file(&mut self, name: &str, data: &[u8]) {
        std::fs::write(self.repo.workdir().unwrap().join(name), data).unwrap();
        let blob_oid = self.repo.write_blob(data).unwrap();
        self.index.entries_mut().push(grit_lib::index::IndexEntry {
            ctime_sec: 0,
            ctime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            dev: 0,
            ino: 0,
            mode: grit_lib::index::MODE_REGULAR,
            uid: 0,
            gid: 0,
            size: 0,
            oid: blob_oid,
            flags: 0,
            flags_extended: None,
            path: name.as_bytes().to_vec(),
            base_index_pos: 0,
        });
    }

    pub fn sync_index(&mut self) {
        self.index.mark_entries_unsorted();
        self.index.sort();
        // Same contract as `update_intent_to_add_impl` in lib/src/git.rs: the
        // entry mutations above leave any loaded cache-tree stale.
        self.index.cache_tree = None;
        self.index.cache_tree_root = None;
        self.repo.index_handle().write(&mut self.index).unwrap();
    }
}

pub fn add_remote(repo_dir: impl AsRef<Path>, remote_name: &str, url: &str) {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["remote", "add", remote_name, url])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git remote add {remote_name} {url} failed with {}:\n{}\n----- stderr -----\n{}",
        output.status,
        BString::from(output.stdout),
        BString::from(output.stderr),
    );
}

pub fn rename_remote(repo_dir: impl AsRef<Path>, original: &str, new: &str) {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["remote", "rename", original, new])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git remote rename failed with {}:\n{}\n----- stderr -----\n{}",
        output.status,
        BString::from(output.stdout),
        BString::from(output.stderr),
    );
}

pub fn fetch(repo_dir: impl AsRef<Path>, remote: &str) {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["fetch", remote])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git fetch {remote} failed with {}:\n{}\n----- stderr -----\n{}",
        output.status,
        BString::from(output.stdout),
        BString::from(output.stderr),
    );
}

pub fn add_worktree(
    repo_dir: impl AsRef<Path>,
    worktree_dir: impl AsRef<OsStr>,
    revision: Option<&str>,
) {
    let mut command = Command::new("git");
    command.current_dir(repo_dir).args(["worktree", "add"]);
    match revision {
        Some(_) => command.arg("--detach"),
        None => command.arg("--orphan"),
    };
    command.arg("--");
    command.arg(worktree_dir);
    command.args(revision);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed with {}:\n{}\n----- stderr -----\n{}",
        output.status,
        BString::from(output.stdout),
        BString::from(output.stderr),
    );
}
