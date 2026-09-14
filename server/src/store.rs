//! The file-backed store behind the API: users and their token hashes,
//! documents with every saved version kept, per-user shares, and review
//! links with their comment threads. One JSON index (`index.json`,
//! rewritten atomically on every change) plus one file per document
//! version under `blobs/<document id>/<version>`. Every rule about who
//! may do what lives here, so it is unit-tested without HTTP.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Why a request was refused. `api` maps these to HTTP statuses.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    NotFound,
    Forbidden,
    Conflict(String),
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "not found"),
            StoreError::Forbidden => write!(f, "not allowed"),
            StoreError::Conflict(m) | StoreError::Invalid(m) | StoreError::Io(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Who is making a request, after their bearer token has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Admin,
    User(String),
}

/// What a share grants: `Edit` is Photoshop's "Can edit" (save new
/// versions, share for review), `View` its "Can view" (open only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Edit,
    View,
}

/// What a user may do with a document: the owner everything, otherwise
/// whatever their share says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Owner,
    Edit,
    View,
}

impl Access {
    fn can_edit(self) -> bool {
        matches!(self, Access::Owner | Access::Edit)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    name: String,
    token_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionRecord {
    pub version: u32,
    pub saved_at: u64,
    pub saved_by: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DocumentRecord {
    id: u64,
    owner: String,
    name: String,
    versions: Vec<VersionRecord>,
    shares: BTreeMap<String, Role>,
}

impl DocumentRecord {
    fn access_for(&self, user: &str) -> Option<Access> {
        if self.owner == user {
            Some(Access::Owner)
        } else {
            self.shares.get(user).map(|role| match role {
                Role::Edit => Access::Edit,
                Role::View => Access::View,
            })
        }
    }

    fn latest(&self) -> &VersionRecord {
        self.versions
            .last()
            .expect("a document always has a version")
    }
}

/// One entry of the document list: `name` is what the client sends back
/// in `GET /documents/{name}` -- the bare name for the user's own
/// documents, `owner/name` for ones shared with them.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocumentSummary {
    pub name: String,
    pub owner: String,
    pub access: Access,
    pub version: u32,
    pub saved_at: u64,
    pub saved_by: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Share {
    pub user: String,
    pub role: Role,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub id: u64,
    pub author: String,
    pub text: String,
    /// Pin position as a fraction of the document's width/height, when
    /// the comment was left on a point of the image rather than on the
    /// document as a whole.
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub parent: Option<u64>,
    pub posted_at: u64,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewRecord {
    id: String,
    document_id: u64,
    version: u32,
    title: String,
    created_by: String,
    created_at: u64,
    comments: Vec<Comment>,
    next_comment_id: u64,
}

/// A review link as anyone holding it sees it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReviewView {
    pub id: String,
    pub document: String,
    pub version: u32,
    pub title: String,
    pub created_by: String,
    pub created_at: u64,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    admin_token_hash: String,
    users: Vec<User>,
    documents: Vec<DocumentRecord>,
    reviews: Vec<ReviewRecord>,
    next_document_id: u64,
}

/// The two tokens minted on first start, shown once and never stored.
pub struct FirstRunTokens {
    pub admin_token: String,
    pub owner_token: String,
}

pub struct Store {
    root: PathBuf,
    index: Mutex<Index>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash_token(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex(&buf)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// A user name: 1-64 characters, letters, digits, `.`, `_`, `-` -- so it
/// can never collide with the `owner/name` document addressing.
pub fn validate_user_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(StoreError::Invalid(
            "a user name is 1 to 64 characters".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(StoreError::Invalid(
            "a user name uses only letters, digits, '.', '_' and '-'".into(),
        ));
    }
    if name == "." || name == ".." {
        return Err(StoreError::Invalid("that user name is reserved".into()));
    }
    Ok(())
}

/// A document name: 1-200 characters, no `/` (that separates owner from
/// name), no control characters, not `.` or `..`.
pub fn validate_document_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().count() > 200 {
        return Err(StoreError::Invalid(
            "a document name is 1 to 200 characters".into(),
        ));
    }
    if name.contains('/') || name.chars().any(char::is_control) {
        return Err(StoreError::Invalid(
            "a document name cannot contain '/' or control characters".into(),
        ));
    }
    if name == "." || name == ".." {
        return Err(StoreError::Invalid("that document name is reserved".into()));
    }
    Ok(())
}

impl Store {
    /// Opens (or creates) the store in `root`. On first creation returns
    /// the freshly minted admin and `owner` tokens.
    pub fn open(root: &Path) -> Result<(Store, Option<FirstRunTokens>)> {
        fs::create_dir_all(root.join("blobs"))?;
        let index_path = root.join("index.json");
        if index_path.exists() {
            let bytes = fs::read(&index_path)?;
            let index: Index = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Io(format!("index.json: {e}")))?;
            return Ok((
                Store {
                    root: root.to_path_buf(),
                    index: Mutex::new(index),
                },
                None,
            ));
        }
        let admin_token = random_hex(32);
        let owner_token = random_hex(32);
        let index = Index {
            admin_token_hash: hash_token(&admin_token),
            users: vec![User {
                name: "owner".into(),
                token_hash: hash_token(&owner_token),
            }],
            documents: Vec::new(),
            reviews: Vec::new(),
            next_document_id: 1,
        };
        let store = Store {
            root: root.to_path_buf(),
            index: Mutex::new(index),
        };
        store.persist(&store.index.lock().expect("index lock"))?;
        write_atomic(&root.join("admin.token"), admin_token.as_bytes())?;
        Ok((
            store,
            Some(FirstRunTokens {
                admin_token,
                owner_token,
            }),
        ))
    }

    fn persist(&self, index: &Index) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(index)
            .map_err(|e| StoreError::Io(format!("serialising index: {e}")))?;
        write_atomic(&self.root.join("index.json"), &bytes)
    }

    fn blob_path(&self, document_id: u64, version: u32) -> PathBuf {
        self.root
            .join("blobs")
            .join(document_id.to_string())
            .join(version.to_string())
    }

    /// The principal a bearer token stands for, if any.
    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        let index = self.index.lock().expect("index lock");
        let hash = hash_token(token);
        if hash == index.admin_token_hash {
            return Some(Principal::Admin);
        }
        index
            .users
            .iter()
            .find(|u| u.token_hash == hash)
            .map(|u| Principal::User(u.name.clone()))
    }

    /// Creates a user and returns their token (shown once).
    pub fn create_user(&self, name: &str) -> Result<String> {
        validate_user_name(name)?;
        let mut index = self.index.lock().expect("index lock");
        if index.users.iter().any(|u| u.name == name) {
            return Err(StoreError::Conflict(format!(
                "user \"{name}\" already exists"
            )));
        }
        let token = random_hex(32);
        index.users.push(User {
            name: name.into(),
            token_hash: hash_token(&token),
        });
        self.persist(&index)?;
        Ok(token)
    }

    /// Replaces a user's token, invalidating the old one.
    pub fn reset_user_token(&self, name: &str) -> Result<String> {
        let mut index = self.index.lock().expect("index lock");
        let token = random_hex(32);
        let user = index
            .users
            .iter_mut()
            .find(|u| u.name == name)
            .ok_or(StoreError::NotFound)?;
        user.token_hash = hash_token(&token);
        self.persist(&index)?;
        Ok(token)
    }

    pub fn list_users(&self) -> Vec<String> {
        let index = self.index.lock().expect("index lock");
        index.users.iter().map(|u| u.name.clone()).collect()
    }

    fn user_exists(index: &Index, name: &str) -> bool {
        index.users.iter().any(|u| u.name == name)
    }

    /// Resolves a document as `user` addresses it: a bare name is their
    /// own; `owner/name` is one shared with them (or their own, spelled
    /// out). Errors `NotFound` for a document the user has no access to
    /// at all, so its existence is not revealed.
    fn resolve<'a>(
        index: &'a Index,
        user: &str,
        addressed: &str,
    ) -> Result<(&'a DocumentRecord, Access)> {
        let (owner, name) = match addressed.split_once('/') {
            Some((owner, name)) => (owner, name),
            None => (user, addressed),
        };
        let record = index
            .documents
            .iter()
            .find(|d| d.owner == owner && d.name == name)
            .ok_or(StoreError::NotFound)?;
        let access = record.access_for(user).ok_or(StoreError::NotFound)?;
        Ok((record, access))
    }

    fn resolve_index(index: &Index, user: &str, addressed: &str) -> Result<(usize, Access)> {
        let (record, access) = Self::resolve(index, user, addressed)?;
        let position = index
            .documents
            .iter()
            .position(|d| d.id == record.id)
            .expect("record came from this list");
        Ok((position, access))
    }

    fn display_name(record: &DocumentRecord, user: &str) -> String {
        if record.owner == user {
            record.name.clone()
        } else {
            format!("{}/{}", record.owner, record.name)
        }
    }

    /// Every document `user` can open: their own, then those shared with
    /// them, each sorted by name.
    pub fn list_documents(&self, user: &str) -> Vec<DocumentSummary> {
        let index = self.index.lock().expect("index lock");
        let mut summaries: Vec<DocumentSummary> = index
            .documents
            .iter()
            .filter_map(|d| {
                let access = d.access_for(user)?;
                let latest = d.latest();
                Some(DocumentSummary {
                    name: Self::display_name(d, user),
                    owner: d.owner.clone(),
                    access,
                    version: latest.version,
                    saved_at: latest.saved_at,
                    saved_by: latest.saved_by.clone(),
                    bytes: latest.bytes,
                })
            })
            .collect();
        summaries.sort_by(|a, b| {
            (a.access != Access::Owner)
                .cmp(&(b.access != Access::Owner))
                .then_with(|| a.name.cmp(&b.name))
        });
        summaries
    }

    /// Saves a new version of `addressed` (creating the document when a
    /// bare name is new). Needs edit access. Returns the version saved.
    pub fn put_document(&self, user: &str, addressed: &str, bytes: &[u8]) -> Result<u32> {
        if bytes.is_empty() {
            return Err(StoreError::Invalid(
                "an empty document cannot be saved".into(),
            ));
        }
        let mut index = self.index.lock().expect("index lock");
        let position = match Self::resolve_index(&index, user, addressed) {
            Ok((position, access)) => {
                if !access.can_edit() {
                    return Err(StoreError::Forbidden);
                }
                position
            }
            Err(StoreError::NotFound) if !addressed.contains('/') => {
                validate_document_name(addressed)?;
                let id = index.next_document_id;
                index.next_document_id += 1;
                index.documents.push(DocumentRecord {
                    id,
                    owner: user.into(),
                    name: addressed.into(),
                    versions: Vec::new(),
                    shares: BTreeMap::new(),
                });
                index.documents.len() - 1
            }
            Err(e) => return Err(e),
        };
        let record = &index.documents[position];
        let version = record.versions.last().map_or(1, |v| v.version + 1);
        let path = self.blob_path(record.id, version);
        fs::create_dir_all(path.parent().expect("blob dir"))?;
        write_atomic(&path, bytes)?;
        index.documents[position].versions.push(VersionRecord {
            version,
            saved_at: now(),
            saved_by: user.into(),
            bytes: bytes.len() as u64,
        });
        self.persist(&index)?;
        Ok(version)
    }

    /// The bytes of `addressed` at `version` (latest when `None`). Any
    /// access suffices.
    pub fn get_document(
        &self,
        user: &str,
        addressed: &str,
        version: Option<u32>,
    ) -> Result<Vec<u8>> {
        let index = self.index.lock().expect("index lock");
        let (record, _) = Self::resolve(&index, user, addressed)?;
        let version = match version {
            Some(v) if record.versions.iter().any(|r| r.version == v) => v,
            Some(_) => return Err(StoreError::NotFound),
            None => record.latest().version,
        };
        Ok(fs::read(self.blob_path(record.id, version))?)
    }

    pub fn list_versions(&self, user: &str, addressed: &str) -> Result<Vec<VersionRecord>> {
        let index = self.index.lock().expect("index lock");
        let (record, _) = Self::resolve(&index, user, addressed)?;
        Ok(record.versions.clone())
    }

    /// Removes a document, every version, its shares and its review
    /// links. Owner only.
    pub fn delete_document(&self, user: &str, addressed: &str) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::resolve_index(&index, user, addressed)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        let record = index.documents.remove(position);
        index.reviews.retain(|r| r.document_id != record.id);
        self.persist(&index)?;
        let dir = self.root.join("blobs").join(record.id.to_string());
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn list_shares(&self, user: &str, addressed: &str) -> Result<Vec<Share>> {
        let index = self.index.lock().expect("index lock");
        let (record, access) = Self::resolve(&index, user, addressed)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        Ok(record
            .shares
            .iter()
            .map(|(user, role)| Share {
                user: user.clone(),
                role: *role,
            })
            .collect())
    }

    /// Invite to Edit: gives `target` (an existing user, not the owner)
    /// `role` on the document. Owner only; re-inviting changes the role.
    pub fn set_share(&self, user: &str, addressed: &str, target: &str, role: Role) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::resolve_index(&index, user, addressed)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        if !Self::user_exists(&index, target) {
            return Err(StoreError::Invalid(format!("no user named \"{target}\"")));
        }
        if index.documents[position].owner == target {
            return Err(StoreError::Invalid(
                "the owner already has full access".into(),
            ));
        }
        index.documents[position].shares.insert(target.into(), role);
        self.persist(&index)
    }

    pub fn remove_share(&self, user: &str, addressed: &str, target: &str) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::resolve_index(&index, user, addressed)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        if index.documents[position].shares.remove(target).is_none() {
            return Err(StoreError::NotFound);
        }
        self.persist(&index)
    }

    /// Share for Review: a link, unguessable by construction (128 random
    /// bits), pinned to the document's current version. Needs edit access.
    pub fn create_review(&self, user: &str, addressed: &str, title: &str) -> Result<ReviewView> {
        let mut index = self.index.lock().expect("index lock");
        let (record, access) = Self::resolve(&index, user, addressed)?;
        if !access.can_edit() {
            return Err(StoreError::Forbidden);
        }
        let review = ReviewRecord {
            id: random_hex(16),
            document_id: record.id,
            version: record.latest().version,
            title: if title.trim().is_empty() {
                record.name.clone()
            } else {
                title.trim().to_string()
            },
            created_by: user.into(),
            created_at: now(),
            comments: Vec::new(),
            next_comment_id: 1,
        };
        let view = Self::review_view(&index, &review);
        index.reviews.push(review);
        self.persist(&index)?;
        Ok(view)
    }

    fn review_view(index: &Index, review: &ReviewRecord) -> ReviewView {
        let document = index
            .documents
            .iter()
            .find(|d| d.id == review.document_id)
            .map_or_else(String::new, |d| d.name.clone());
        ReviewView {
            id: review.id.clone(),
            document,
            version: review.version,
            title: review.title.clone(),
            created_by: review.created_by.clone(),
            created_at: review.created_at,
            comments: review.comments.clone(),
        }
    }

    pub fn list_reviews(&self, user: &str, addressed: &str) -> Result<Vec<ReviewView>> {
        let index = self.index.lock().expect("index lock");
        let (record, access) = Self::resolve(&index, user, addressed)?;
        if !access.can_edit() {
            return Err(StoreError::Forbidden);
        }
        Ok(index
            .reviews
            .iter()
            .filter(|r| r.document_id == record.id)
            .map(|r| Self::review_view(&index, r))
            .collect())
    }

    /// A review as anyone holding its link sees it -- no token needed;
    /// the link itself is the credential.
    pub fn get_review(&self, id: &str) -> Result<ReviewView> {
        let index = self.index.lock().expect("index lock");
        let review = index
            .reviews
            .iter()
            .find(|r| r.id == id)
            .ok_or(StoreError::NotFound)?;
        Ok(Self::review_view(&index, review))
    }

    /// The exact version the review was shared at, even after the
    /// document has moved on.
    pub fn get_review_document(&self, id: &str) -> Result<Vec<u8>> {
        let index = self.index.lock().expect("index lock");
        let review = index
            .reviews
            .iter()
            .find(|r| r.id == id)
            .ok_or(StoreError::NotFound)?;
        Ok(fs::read(
            self.blob_path(review.document_id, review.version),
        )?)
    }

    /// Anyone with the link can comment, under any display name -- as
    /// with Photoshop's own review links, reviewers need no account.
    pub fn add_comment(
        &self,
        id: &str,
        author: &str,
        text: &str,
        x: Option<f64>,
        y: Option<f64>,
        parent: Option<u64>,
    ) -> Result<Comment> {
        let author = author.trim();
        let text = text.trim();
        if author.is_empty() || author.chars().count() > 80 {
            return Err(StoreError::Invalid(
                "a comment needs an author name of up to 80 characters".into(),
            ));
        }
        if text.is_empty() || text.chars().count() > 4000 {
            return Err(StoreError::Invalid(
                "a comment is 1 to 4000 characters".into(),
            ));
        }
        if let (Some(x), Some(y)) = (x, y) {
            if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
                return Err(StoreError::Invalid(
                    "a pin is placed at fractions 0..1 of the image".into(),
                ));
            }
        } else if x.is_some() || y.is_some() {
            return Err(StoreError::Invalid("a pin needs both x and y".into()));
        }
        let mut index = self.index.lock().expect("index lock");
        let review = index
            .reviews
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or(StoreError::NotFound)?;
        if let Some(parent) = parent {
            let parent_comment = review
                .comments
                .iter()
                .find(|c| c.id == parent)
                .ok_or_else(|| StoreError::Invalid("no such comment to reply to".into()))?;
            if parent_comment.parent.is_some() {
                return Err(StoreError::Invalid(
                    "replies go on the thread's first comment".into(),
                ));
            }
        }
        let comment = Comment {
            id: review.next_comment_id,
            author: author.into(),
            text: text.into(),
            x,
            y,
            parent,
            posted_at: now(),
            resolved: false,
        };
        review.next_comment_id += 1;
        review.comments.push(comment.clone());
        self.persist(&index)?;
        Ok(comment)
    }

    /// Marks a thread resolved (or open again). Needs edit access to the
    /// document the review is of.
    pub fn set_comment_resolved(
        &self,
        user: &str,
        id: &str,
        comment_id: u64,
        resolved: bool,
    ) -> Result<Comment> {
        let mut index = self.index.lock().expect("index lock");
        let review_position = index
            .reviews
            .iter()
            .position(|r| r.id == id)
            .ok_or(StoreError::NotFound)?;
        let document_id = index.reviews[review_position].document_id;
        let record = index
            .documents
            .iter()
            .find(|d| d.id == document_id)
            .ok_or(StoreError::NotFound)?;
        if !record.access_for(user).is_some_and(Access::can_edit) {
            return Err(StoreError::Forbidden);
        }
        let comment = index.reviews[review_position]
            .comments
            .iter_mut()
            .find(|c| c.id == comment_id)
            .ok_or(StoreError::NotFound)?;
        comment.resolved = resolved;
        let comment = comment.clone();
        self.persist(&index)?;
        Ok(comment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, Store, FirstRunTokens) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, tokens) = Store::open(dir.path()).expect("open");
        (dir, store, tokens.expect("first run"))
    }

    #[test]
    fn first_start_mints_tokens_and_stores_only_hashes() {
        let (dir, store, tokens) = fresh();
        assert_eq!(
            store.authenticate(&tokens.admin_token),
            Some(Principal::Admin)
        );
        assert_eq!(
            store.authenticate(&tokens.owner_token),
            Some(Principal::User("owner".into()))
        );
        assert_eq!(store.authenticate("nope"), None);
        let index = fs::read_to_string(dir.path().join("index.json")).unwrap();
        assert!(!index.contains(&tokens.admin_token));
        assert!(!index.contains(&tokens.owner_token));
        assert_eq!(
            fs::read_to_string(dir.path().join("admin.token")).unwrap(),
            tokens.admin_token
        );
        // A second open is not a first run and keeps the same credentials.
        let (again, first) = Store::open(dir.path()).unwrap();
        assert!(first.is_none());
        assert_eq!(
            again.authenticate(&tokens.owner_token),
            Some(Principal::User("owner".into()))
        );
    }

    #[test]
    fn users_are_created_once_and_tokens_rotate() {
        let (_dir, store, _) = fresh();
        let token = store.create_user("ana").unwrap();
        assert_eq!(
            store.authenticate(&token),
            Some(Principal::User("ana".into()))
        );
        assert!(matches!(
            store.create_user("ana"),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.create_user("bad name"),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.create_user("a/b"),
            Err(StoreError::Invalid(_))
        ));
        let rotated = store.reset_user_token("ana").unwrap();
        assert_eq!(store.authenticate(&token), None);
        assert_eq!(
            store.authenticate(&rotated),
            Some(Principal::User("ana".into()))
        );
        assert_eq!(store.reset_user_token("zed"), Err(StoreError::NotFound));
        assert_eq!(
            store.list_users(),
            vec!["owner".to_string(), "ana".to_string()]
        );
    }

    #[test]
    fn documents_keep_every_version_and_survive_reopen() {
        let (dir, store, _) = fresh();
        assert_eq!(store.put_document("owner", "poster", b"v1").unwrap(), 1);
        assert_eq!(
            store.put_document("owner", "poster", b"v2 bytes").unwrap(),
            2
        );
        assert_eq!(
            store.get_document("owner", "poster", None).unwrap(),
            b"v2 bytes"
        );
        assert_eq!(
            store.get_document("owner", "poster", Some(1)).unwrap(),
            b"v1"
        );
        assert_eq!(
            store.get_document("owner", "poster", Some(3)),
            Err(StoreError::NotFound)
        );
        let versions = store.list_versions("owner", "poster").unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].version, 2);
        assert_eq!(versions[1].bytes, 8);
        assert_eq!(versions[1].saved_by, "owner");
        assert!(matches!(
            store.put_document("owner", "x", b""),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.put_document("owner", "a/b", b"x"),
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.put_document("owner", "..", b"x"),
            Err(StoreError::Invalid(_))
        ));
        drop(store);
        let (reopened, _) = Store::open(dir.path()).unwrap();
        assert_eq!(
            reopened.get_document("owner", "poster", None).unwrap(),
            b"v2 bytes"
        );
        let list = reopened.list_documents("owner");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "poster");
        assert_eq!(list[0].access, Access::Owner);
        assert_eq!(list[0].version, 2);
    }

    #[test]
    fn shares_grant_exactly_their_role() {
        let (_dir, store, _) = fresh();
        store.create_user("ana").unwrap();
        store.create_user("bo").unwrap();
        store.put_document("owner", "poster", b"v1").unwrap();
        // Nothing is visible before a share, not even that it exists.
        assert_eq!(
            store.get_document("ana", "owner/poster", None),
            Err(StoreError::NotFound)
        );
        assert!(store.list_documents("ana").is_empty());

        store
            .set_share("owner", "poster", "ana", Role::View)
            .unwrap();
        store
            .set_share("owner", "poster", "bo", Role::Edit)
            .unwrap();
        assert_eq!(
            store.get_document("ana", "owner/poster", None).unwrap(),
            b"v1"
        );
        assert_eq!(
            store.put_document("ana", "owner/poster", b"v2"),
            Err(StoreError::Forbidden)
        );
        assert_eq!(store.put_document("bo", "owner/poster", b"v2").unwrap(), 2);
        assert_eq!(
            store.list_versions("ana", "owner/poster").unwrap()[1].saved_by,
            "bo"
        );
        let ana_list = store.list_documents("ana");
        assert_eq!(ana_list[0].name, "owner/poster");
        assert_eq!(ana_list[0].access, Access::View);
        // Only the owner manages shares or deletes.
        assert_eq!(
            store.set_share("bo", "owner/poster", "ana", Role::Edit),
            Err(StoreError::Forbidden)
        );
        assert_eq!(
            store.list_shares("bo", "owner/poster"),
            Err(StoreError::Forbidden)
        );
        assert_eq!(
            store.delete_document("bo", "owner/poster"),
            Err(StoreError::Forbidden)
        );
        assert!(matches!(
            store.set_share("owner", "poster", "nobody", Role::Edit),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.set_share("owner", "poster", "owner", Role::Edit),
            Err(StoreError::Invalid(_))
        ));
        // Re-inviting changes the role; removing takes it away entirely.
        store
            .set_share("owner", "poster", "ana", Role::Edit)
            .unwrap();
        assert_eq!(
            store.list_shares("owner", "poster").unwrap(),
            vec![
                Share {
                    user: "ana".into(),
                    role: Role::Edit
                },
                Share {
                    user: "bo".into(),
                    role: Role::Edit
                }
            ]
        );
        store.remove_share("owner", "poster", "ana").unwrap();
        assert_eq!(
            store.remove_share("owner", "poster", "ana"),
            Err(StoreError::NotFound)
        );
        assert_eq!(
            store.get_document("ana", "owner/poster", None),
            Err(StoreError::NotFound)
        );
        // Own documents also resolve spelled out.
        assert_eq!(
            store.get_document("owner", "owner/poster", None).unwrap(),
            b"v2"
        );
        // The list puts one's own documents first, then shared ones by name.
        store.put_document("bo", "zeta", b"z").unwrap();
        let names: Vec<_> = store
            .list_documents("bo")
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, vec!["zeta", "owner/poster"]);
    }

    #[test]
    fn review_links_pin_a_version_and_take_threaded_comments() {
        let (_dir, store, _) = fresh();
        store.create_user("ana").unwrap();
        store.put_document("owner", "poster", b"v1").unwrap();
        store
            .set_share("owner", "poster", "ana", Role::View)
            .unwrap();
        assert_eq!(
            store.create_review("ana", "owner/poster", ""),
            Err(StoreError::Forbidden)
        );
        let review = store.create_review("owner", "poster", "  ").unwrap();
        assert_eq!(review.id.len(), 32);
        assert_eq!(review.title, "poster");
        assert_eq!(review.version, 1);
        assert_eq!(review.document, "poster");
        // A later save does not change what the link shows.
        store.put_document("owner", "poster", b"v2").unwrap();
        assert_eq!(store.get_review_document(&review.id).unwrap(), b"v1");
        assert_eq!(store.get_review("missing"), Err(StoreError::NotFound));

        let first = store
            .add_comment(
                &review.id,
                "  A reviewer ",
                " Too dark on the left ",
                Some(0.1),
                Some(0.5),
                None,
            )
            .unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(first.author, "A reviewer");
        assert_eq!(first.text, "Too dark on the left");
        let reply = store
            .add_comment(&review.id, "owner", "Fixed in v2", None, None, Some(1))
            .unwrap();
        assert_eq!(reply.parent, Some(1));
        assert!(matches!(
            store.add_comment(&review.id, "x", "nested", None, None, Some(reply.id)),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.add_comment(&review.id, "", "no author", None, None, None),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.add_comment(&review.id, "a", "half pin", Some(0.5), None, None),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.add_comment(&review.id, "a", "off image", Some(1.5), Some(0.5), None),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.add_comment(&review.id, "a", "no thread", None, None, Some(99)),
            Err(StoreError::Invalid(_))
        ));

        // Resolving needs edit access to the document, not just the link.
        assert_eq!(
            store.set_comment_resolved("ana", &review.id, 1, true),
            Err(StoreError::Forbidden)
        );
        assert!(
            store
                .set_comment_resolved("owner", &review.id, 1, true)
                .unwrap()
                .resolved
        );
        assert_eq!(
            store.set_comment_resolved("owner", &review.id, 7, true),
            Err(StoreError::NotFound)
        );
        let view = store.get_review(&review.id).unwrap();
        assert_eq!(view.comments.len(), 2);
        assert!(view.comments[0].resolved);
        assert!(!view.comments[1].resolved);
        assert_eq!(store.list_reviews("owner", "poster").unwrap().len(), 1);
        assert_eq!(
            store.list_reviews("ana", "owner/poster"),
            Err(StoreError::Forbidden)
        );

        // Deleting the document takes its reviews and blobs with it.
        store.delete_document("owner", "poster").unwrap();
        assert_eq!(store.get_review(&review.id), Err(StoreError::NotFound));
        assert_eq!(
            store.get_document("owner", "poster", None),
            Err(StoreError::NotFound)
        );
        assert!(store.list_documents("owner").is_empty());
    }

    #[test]
    fn name_rules() {
        assert!(validate_document_name("Poster (final) v2.imgproj").is_ok());
        assert!(validate_document_name("日本語").is_ok());
        assert!(validate_document_name("").is_err());
        assert!(validate_document_name("a/b").is_err());
        assert!(validate_document_name("a\nb").is_err());
        assert!(validate_document_name(".").is_err());
        assert!(validate_document_name(&"x".repeat(201)).is_err());
        assert!(validate_user_name("ana.b_c-d").is_ok());
        assert!(validate_user_name("ana b").is_err());
        assert!(validate_user_name("..").is_err());
        assert!(validate_user_name(&"a".repeat(65)).is_err());
    }
}
