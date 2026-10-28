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

//! Helpers for reading and writing Git refs and objects with grit-lib.

use std::ops::ControlFlow;

use grit_lib::objects::ObjectId as GitOid;
use grit_lib::objects::ObjectKind;
use grit_lib::refs::store::Expected;
use grit_lib::refs::store::RawRef;
use grit_lib::refs::store::RefStoreError;
use grit_lib::refs::store::RefTransaction;
use grit_lib::refs::store::RefUpdate;
use grit_lib::refs::store::RefUpdateFlags;
use grit_lib::refs::store::ReflogUpdate;
use grit_lib::repo::Repository;

use crate::object_id::ObjectId;

/// Error from a grit-lib call.
pub(crate) type GritError = grit_lib::error::Error;

/// Converts an id of valid length to a Git object id.
pub(crate) fn to_git_oid(id: &impl ObjectId) -> GitOid {
    GitOid::from_bytes(id.as_bytes()).expect("object id should have valid length")
}

/// A ref as stored, without resolving symbolic targets.
#[derive(Clone, Debug)]
pub(crate) struct GitRef {
    pub name: String,
    pub target: RawRef,
}

/// Lists the refs whose names start with `prefix`, in sorted order.
pub(crate) fn list_refs(repo: &Repository, prefix: &str) -> Result<Vec<GitRef>, GritError> {
    let mut refs = Vec::new();
    repo.refs().for_each_ref(prefix, &mut |entry| {
        refs.push(GitRef {
            name: entry.name.clone(),
            target: entry.value.clone(),
        });
        ControlFlow::Continue(())
    })?;
    Ok(refs)
}

/// Reads the stored value of the ref `name`.
pub(crate) fn find_ref(repo: &Repository, name: &str) -> Result<Option<RawRef>, GritError> {
    repo.refs().read_raw(name)
}

/// Resolves `target` through symbolic refs. Returns `None` if a ref in the
/// chain doesn't exist.
pub(crate) fn resolve_target(repo: &Repository, target: &RawRef) -> Option<GitOid> {
    match target {
        RawRef::Direct(oid) => Some(*oid),
        RawRef::Symbolic(name) => repo.refs().resolve(name).ok(),
    }
}

/// Follows annotated tags from `oid`, and returns the id of the commit at the
/// end. Returns `None` if the object chain ends at a non-commit object or an
/// object is missing.
pub(crate) fn peel_to_commit(repo: &Repository, oid: GitOid) -> Option<GitOid> {
    let mut oid = oid;
    // Tag chains are short, but guard against cycles of corrupt objects.
    for _ in 0..64 {
        let object = repo.odb().read(&oid).ok()?;
        match object.kind {
            ObjectKind::Commit => return Some(oid),
            ObjectKind::Tag => {
                oid = grit_lib::objects::tag_object_line_oid(&object.data)?;
            }
            _ => return None,
        }
    }
    None
}

/// Builds a reflog record for an update of `name` the way Git would: only if
/// the ref already has a reflog or `core.logAllRefUpdates` asks for one.
pub(crate) fn reflog_for(
    repo: &Repository,
    identity: &str,
    name: &str,
    message: &str,
) -> Option<ReflogUpdate> {
    let has_reflog = repo.refs().reflog_exists(name).unwrap_or(false);
    let autocreate = grit_lib::refs::should_autocreate_reflog(repo.git_dir(), name);
    (has_reflog || autocreate).then(|| ReflogUpdate {
        identity: identity.to_owned(),
        message: message.to_owned(),
        time: repo.identity_now(),
        old_oid: None,
        new_oid: None,
    })
}

/// Builds an update setting `name` to `new_value` (or deleting it if `None`).
pub(crate) fn ref_update(
    name: impl Into<String>,
    new_value: Option<RawRef>,
    expected: Expected,
    reflog: Option<ReflogUpdate>,
) -> RefUpdate {
    RefUpdate {
        name: name.into(),
        new_value,
        expected,
        reflog,
        flags: RefUpdateFlags {
            no_deref: true,
            log_only: false,
            ..RefUpdateFlags::default()
        },
    }
}

/// Applies the given ref updates in one transaction. Like Git, deleted refs
/// lose their reflogs.
pub(crate) fn edit_refs(
    repo: &Repository,
    updates: impl IntoIterator<Item = RefUpdate>,
) -> Result<(), RefStoreError> {
    let mut transaction = RefTransaction::new();
    let mut deleted = Vec::new();
    for update in updates {
        if update.new_value.is_none() && !update.flags.log_only {
            deleted.push(update.name.clone());
        }
        transaction.push_update(update)?;
    }
    if transaction.updates().is_empty() {
        return Ok(());
    }
    repo.refs().prepare(transaction)?.commit()?;
    for name in deleted {
        repo.refs()
            .delete_reflog(&name)
            .map_err(RefStoreError::backend)?;
    }
    Ok(())
}
