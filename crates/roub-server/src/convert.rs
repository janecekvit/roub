//! Conversions between the wire types (`roub_proto::v1`) and `roub-core`. Every value from the
//! wire is validated here, and a bad value becomes an `INVALID_ARGUMENT` naming the field.

use roub_core::{
    ChangesetFields, CopyFrom, EntryMode, FileChange, FileOp, ObjectId, ObjectKind, RefName,
    RepoPath, RepositoryName, Signature,
};
use roub_proto::v1 as proto;
use tonic::Status;

pub(crate) fn repository(name: &str) -> Result<RepositoryName, Status> {
    RepositoryName::new(name)
        .map_err(|e| Status::invalid_argument(format!("repository '{name}': {e}")))
}

pub(crate) fn ref_name(name: &str) -> Result<RefName, Status> {
    RefName::new(name).map_err(|e| Status::invalid_argument(format!("ref '{name}': {e}")))
}

/// A required id.
pub(crate) fn object_id(id: Option<&proto::ObjectId>, field: &str) -> Result<ObjectId, Status> {
    optional_object_id(id, field)?
        .ok_or_else(|| Status::invalid_argument(format!("{field} is missing")))
}

/// An id whose absence means something, such as "no ref yet".
pub(crate) fn optional_object_id(
    id: Option<&proto::ObjectId>,
    field: &str,
) -> Result<Option<ObjectId>, Status> {
    id.map(|id| {
        ObjectId::from_slice(&id.digest)
            .map_err(|e| Status::invalid_argument(format!("{field}: {e}")))
    })
    .transpose()
}

pub(crate) fn proto_id(id: &ObjectId) -> proto::ObjectId {
    proto::ObjectId {
        digest: id.as_bytes().to_vec(),
    }
}

pub(crate) fn proto_kind(kind: ObjectKind) -> proto::ObjectKind {
    match kind {
        ObjectKind::Blob => proto::ObjectKind::Blob,
        ObjectKind::Tree => proto::ObjectKind::Tree,
        ObjectKind::Changeset => proto::ObjectKind::Changeset,
    }
}

/// The changeset fields of a request, unsorted and not yet checked against the changeset rules;
/// `Changeset::new` does both.
pub(crate) fn changeset_fields(
    request: proto::CreateChangesetRequest,
) -> Result<ChangesetFields, Status> {
    let parents = request
        .parents
        .iter()
        .enumerate()
        .map(|(index, id)| object_id(Some(id), &format!("parents[{index}]")))
        .collect::<Result<_, _>>()?;
    let author = request
        .author
        .map(signature)
        .ok_or_else(|| Status::invalid_argument("author is missing"))?;
    let committer = request.committer.map(signature);
    let changes = request
        .changes
        .into_iter()
        .map(file_change)
        .collect::<Result<_, _>>()?;
    Ok(ChangesetFields {
        parents,
        author,
        committer,
        message: request.message,
        changes,
        extensions: Vec::new(),
    })
}

/// `Changeset::new` checks the values.
fn signature(signature: proto::Signature) -> Signature {
    Signature {
        name: signature.name,
        email: signature.email,
        time: signature.time,
        tz_offset_minutes: signature.tz_offset_minutes,
    }
}

fn file_change(change: proto::FileChange) -> Result<FileChange, Status> {
    let path = repo_path(&change.path, "changes.path")?;
    let op = match change.change {
        Some(proto::file_change::Change::Put(content)) => {
            let mode = match proto::FileMode::try_from(content.mode) {
                Ok(proto::FileMode::Regular) => EntryMode::Regular,
                Ok(proto::FileMode::Executable) => EntryMode::Executable,
                Ok(proto::FileMode::Symlink) => EntryMode::Symlink,
                Ok(proto::FileMode::Unspecified) | Err(_) => {
                    return Err(Status::invalid_argument(format!(
                        "'{path}': file mode {} is not a valid mode",
                        content.mode
                    )));
                }
            };
            let blob = object_id(content.blob.as_ref(), &format!("'{path}': blob"))?;
            let copy_from = content
                .copy_from
                .map(|copy| {
                    Ok::<_, Status>(CopyFrom {
                        parent: copy.parent as usize,
                        path: repo_path(&copy.path, &format!("'{path}': copy_from.path"))?,
                    })
                })
                .transpose()?;
            FileOp::Put {
                mode,
                blob,
                size: content.size,
                copy_from,
            }
        }
        Some(proto::file_change::Change::Delete(proto::Deleted {})) => FileOp::Delete,
        None => {
            return Err(Status::invalid_argument(format!(
                "'{path}': the change is neither a put nor a delete"
            )));
        }
    };
    Ok(FileChange { path, op })
}

fn repo_path(path: &str, field: &str) -> Result<RepoPath, Status> {
    RepoPath::new(path).map_err(|e| Status::invalid_argument(format!("{field} '{path}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    fn put(path: &str, mode: proto::FileMode) -> proto::FileChange {
        proto::FileChange {
            path: path.into(),
            change: Some(proto::file_change::Change::Put(proto::FileContent {
                blob: Some(proto_id(&ObjectId::from_bytes([7; ObjectId::LEN]))),
                mode: mode.into(),
                size: 1,
                copy_from: None,
            })),
        }
    }

    fn request(changes: Vec<proto::FileChange>) -> proto::CreateChangesetRequest {
        proto::CreateChangesetRequest {
            repository: "repo".into(),
            changes,
            message: "m".into(),
            parents: vec![],
            author: Some(proto::Signature::default()),
            committer: None,
        }
    }

    fn error(request: proto::CreateChangesetRequest) -> String {
        let status = changeset_fields(request).unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument);
        status.message().to_owned()
    }

    #[test]
    fn converts_a_valid_request() {
        let fields = changeset_fields(request(vec![
            put("a", proto::FileMode::Executable),
            proto::FileChange {
                path: "b".into(),
                change: Some(proto::file_change::Change::Delete(proto::Deleted {})),
            },
        ]))
        .unwrap();
        assert_eq!(fields.changes.len(), 2);
        assert!(matches!(
            fields.changes[0].op,
            FileOp::Put {
                mode: EntryMode::Executable,
                ..
            }
        ));
        assert_eq!(fields.changes[1].op, FileOp::Delete);
    }

    #[test]
    fn rejects_invalid_fields() {
        let mut no_author = request(vec![]);
        no_author.author = None;
        assert_eq!(error(no_author), "author is missing");

        assert!(
            error(request(vec![put("a", proto::FileMode::Unspecified)])).contains("file mode 0")
        );
        let mut unknown_mode = put("a", proto::FileMode::Regular);
        if let Some(proto::file_change::Change::Put(content)) = &mut unknown_mode.change {
            content.mode = 42;
        }
        assert!(error(request(vec![unknown_mode])).contains("file mode 42"));

        assert!(error(request(vec![put("a/../b", proto::FileMode::Regular)])).contains("a/../b"));

        let mut short_id = put("a", proto::FileMode::Regular);
        if let Some(proto::file_change::Change::Put(content)) = &mut short_id.change {
            content.blob = Some(proto::ObjectId { digest: vec![1, 2] });
        }
        assert!(error(request(vec![short_id])).starts_with("'a': blob"));

        let neither = proto::FileChange {
            path: "a".into(),
            change: None,
        };
        assert!(error(request(vec![neither])).contains("neither"));

        let mut bad_parent = request(vec![]);
        bad_parent.parents = vec![proto::ObjectId { digest: vec![] }];
        assert!(error(bad_parent).starts_with("parents[0]"));
    }

    #[test]
    fn validates_names_and_ids() {
        assert_eq!(repository("a/b").unwrap().as_str(), "a/b");
        assert_eq!(
            repository("../x").unwrap_err().code(),
            Code::InvalidArgument
        );
        assert_eq!(ref_name("").unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(optional_object_id(None, "x").unwrap(), None);
        assert_eq!(object_id(None, "x").unwrap_err().message(), "x is missing");
        let id = ObjectId::from_bytes([3; ObjectId::LEN]);
        assert_eq!(object_id(Some(&proto_id(&id)), "x").unwrap(), id);
    }
}
