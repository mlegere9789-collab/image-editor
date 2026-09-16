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

fn access_of(owner: &str, shares: &BTreeMap<String, Role>, user: &str) -> Option<Access> {
    if owner == user {
        Some(Access::Owner)
    } else {
        shares.get(user).map(|role| match role {
            Role::Edit => Access::Edit,
            Role::View => Access::View,
        })
    }
}

impl DocumentRecord {
    fn access_for(&self, user: &str) -> Option<Access> {
        access_of(&self.owner, &self.shares, user)
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

/// One item of a library: a small JSON-described asset (a colour, a
/// gradient, an adjustment preset) or a graphic whose PNG bytes live in
/// a blob file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub data: serde_json::Value,
    pub bytes: u64,
    pub added_by: String,
    pub added_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LibraryRecord {
    id: u64,
    owner: String,
    name: String,
    shares: BTreeMap<String, Role>,
    assets: Vec<Asset>,
    next_asset_id: u64,
}

impl LibraryRecord {
    fn access_for(&self, user: &str) -> Option<Access> {
        access_of(&self.owner, &self.shares, user)
    }
}

/// A library as its user sees it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LibrarySummary {
    pub id: u64,
    pub name: String,
    pub owner: String,
    pub access: Access,
    pub assets: usize,
}

pub const ASSET_KINDS: [&str; 4] = ["color", "gradient", "adjustment", "graphic"];

/// One thing pinned to a board: an image (its PNG in a blob file), a
/// note, or a prompt -- at a position and size on the board's canvas.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoardItem {
    pub id: u64,
    pub kind: String,
    pub name: String,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub bytes: u64,
    pub added_by: String,
    pub added_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoardRecord {
    id: u64,
    owner: String,
    name: String,
    shares: BTreeMap<String, Role>,
    items: Vec<BoardItem>,
    next_item_id: u64,
}

impl BoardRecord {
    fn access_for(&self, user: &str) -> Option<Access> {
        access_of(&self.owner, &self.shares, user)
    }
}

/// A board as its user sees it in the list.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BoardSummary {
    pub id: u64,
    pub name: String,
    pub owner: String,
    pub access: Access,
    pub items: usize,
}

pub const BOARD_ITEM_KINDS: [&str; 3] = ["image", "note", "prompt"];

/// A change to a board item: any subset of its position, size and text.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BoardItemPatch {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    pub text: Option<String>,
    pub name: Option<String>,
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
    #[serde(default)]
    libraries: Vec<LibraryRecord>,
    #[serde(default = "one")]
    next_library_id: u64,
    #[serde(default)]
    boards: Vec<BoardRecord>,
    #[serde(default = "one")]
    next_board_id: u64,
}

fn one() -> u64 {
    1
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
            libraries: Vec::new(),
            next_library_id: 1,
            boards: Vec::new(),
            next_board_id: 1,
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

impl Store {
    fn asset_blob_path(&self, library_id: u64, asset_id: u64) -> PathBuf {
        self.root
            .join("blobs")
            .join(format!("lib-{library_id}"))
            .join(asset_id.to_string())
    }

    fn library_index(index: &Index, user: &str, id: u64) -> Result<(usize, Access)> {
        let position = index
            .libraries
            .iter()
            .position(|l| l.id == id)
            .ok_or(StoreError::NotFound)?;
        let access = index.libraries[position]
            .access_for(user)
            .ok_or(StoreError::NotFound)?;
        Ok((position, access))
    }

    /// Every library `user` can open: their own first, then shared ones.
    pub fn list_libraries(&self, user: &str) -> Vec<LibrarySummary> {
        let index = self.index.lock().expect("index lock");
        let mut list: Vec<LibrarySummary> = index
            .libraries
            .iter()
            .filter_map(|l| {
                Some(LibrarySummary {
                    id: l.id,
                    name: l.name.clone(),
                    owner: l.owner.clone(),
                    access: l.access_for(user)?,
                    assets: l.assets.len(),
                })
            })
            .collect();
        list.sort_by(|a, b| {
            (a.access != Access::Owner)
                .cmp(&(b.access != Access::Owner))
                .then_with(|| a.name.cmp(&b.name))
        });
        list
    }

    pub fn create_library(&self, user: &str, name: &str) -> Result<LibrarySummary> {
        let name = name.trim();
        validate_document_name(name)?;
        let mut index = self.index.lock().expect("index lock");
        if index
            .libraries
            .iter()
            .any(|l| l.owner == user && l.name == name)
        {
            return Err(StoreError::Conflict(format!(
                "you already have a library named \"{name}\""
            )));
        }
        let id = index.next_library_id;
        index.next_library_id += 1;
        index.libraries.push(LibraryRecord {
            id,
            owner: user.into(),
            name: name.into(),
            shares: BTreeMap::new(),
            assets: Vec::new(),
            next_asset_id: 1,
        });
        self.persist(&index)?;
        Ok(LibrarySummary {
            id,
            name: name.into(),
            owner: user.into(),
            access: Access::Owner,
            assets: 0,
        })
    }

    pub fn delete_library(&self, user: &str, id: u64) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::library_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        index.libraries.remove(position);
        self.persist(&index)?;
        let dir = self.root.join("blobs").join(format!("lib-{id}"));
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn list_library_shares(&self, user: &str, id: u64) -> Result<Vec<Share>> {
        let index = self.index.lock().expect("index lock");
        let (position, access) = Self::library_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        Ok(index.libraries[position]
            .shares
            .iter()
            .map(|(user, role)| Share {
                user: user.clone(),
                role: *role,
            })
            .collect())
    }

    pub fn set_library_share(&self, user: &str, id: u64, target: &str, role: Role) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::library_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        if !Self::user_exists(&index, target) {
            return Err(StoreError::Invalid(format!("no user named \"{target}\"")));
        }
        if index.libraries[position].owner == target {
            return Err(StoreError::Invalid(
                "the owner already has full access".into(),
            ));
        }
        index.libraries[position].shares.insert(target.into(), role);
        self.persist(&index)
    }

    pub fn remove_library_share(&self, user: &str, id: u64, target: &str) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::library_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        if index.libraries[position].shares.remove(target).is_none() {
            return Err(StoreError::NotFound);
        }
        self.persist(&index)
    }

    pub fn list_assets(&self, user: &str, id: u64) -> Result<Vec<Asset>> {
        let index = self.index.lock().expect("index lock");
        let (position, _) = Self::library_index(&index, user, id)?;
        Ok(index.libraries[position].assets.clone())
    }

    /// Adds an asset: `data` describes it for every kind but `graphic`,
    /// whose PNG goes in `blob`. Needs edit access. Names are unique
    /// within a kind, so re-adding replaces.
    pub fn add_asset(
        &self,
        user: &str,
        id: u64,
        name: &str,
        kind: &str,
        data: serde_json::Value,
        blob: Option<&[u8]>,
    ) -> Result<Asset> {
        let name = name.trim();
        validate_document_name(name)?;
        if !ASSET_KINDS.contains(&kind) {
            return Err(StoreError::Invalid(format!(
                "an asset is one of {}",
                ASSET_KINDS.join(", ")
            )));
        }
        if (kind == "graphic") != blob.is_some() {
            return Err(StoreError::Invalid(
                "a graphic is its PNG bytes; every other kind is JSON data".into(),
            ));
        }
        if blob.is_some_and(<[u8]>::is_empty) {
            return Err(StoreError::Invalid(
                "an empty graphic cannot be added".into(),
            ));
        }
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::library_index(&index, user, id)?;
        if !access.can_edit() {
            return Err(StoreError::Forbidden);
        }
        let library = &mut index.libraries[position];
        if let Some(existing) = library
            .assets
            .iter()
            .position(|a| a.kind == kind && a.name == name)
        {
            let old = library.assets.remove(existing);
            let path = self.asset_blob_path(id, old.id);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        let asset_id = library.next_asset_id;
        library.next_asset_id += 1;
        if let Some(bytes) = blob {
            let path = self.asset_blob_path(id, asset_id);
            fs::create_dir_all(path.parent().expect("blob dir"))?;
            write_atomic(&path, bytes)?;
        }
        let asset = Asset {
            id: asset_id,
            name: name.into(),
            kind: kind.into(),
            data,
            bytes: blob.map_or(0, |b| b.len() as u64),
            added_by: user.into(),
            added_at: now(),
        };
        library.assets.push(asset.clone());
        self.persist(&index)?;
        Ok(asset)
    }

    pub fn get_asset_blob(&self, user: &str, id: u64, asset_id: u64) -> Result<Vec<u8>> {
        let index = self.index.lock().expect("index lock");
        let (position, _) = Self::library_index(&index, user, id)?;
        let asset = index.libraries[position]
            .assets
            .iter()
            .find(|a| a.id == asset_id)
            .ok_or(StoreError::NotFound)?;
        if asset.kind != "graphic" {
            return Err(StoreError::NotFound);
        }
        Ok(fs::read(self.asset_blob_path(id, asset_id))?)
    }

    pub fn delete_asset(&self, user: &str, id: u64, asset_id: u64) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::library_index(&index, user, id)?;
        if !access.can_edit() {
            return Err(StoreError::Forbidden);
        }
        let library = &mut index.libraries[position];
        let at = library
            .assets
            .iter()
            .position(|a| a.id == asset_id)
            .ok_or(StoreError::NotFound)?;
        library.assets.remove(at);
        self.persist(&index)?;
        let path = self.asset_blob_path(id, asset_id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl Store {
    fn board_blob_path(&self, board_id: u64, item_id: u64) -> PathBuf {
        self.root
            .join("blobs")
            .join(format!("board-{board_id}"))
            .join(item_id.to_string())
    }

    fn board_index(index: &Index, user: &str, id: u64) -> Result<(usize, Access)> {
        let position = index
            .boards
            .iter()
            .position(|b| b.id == id)
            .ok_or(StoreError::NotFound)?;
        let access = index.boards[position]
            .access_for(user)
            .ok_or(StoreError::NotFound)?;
        Ok((position, access))
    }

    pub fn list_boards(&self, user: &str) -> Vec<BoardSummary> {
        let index = self.index.lock().expect("index lock");
        let mut list: Vec<BoardSummary> = index
            .boards
            .iter()
            .filter_map(|b| {
                Some(BoardSummary {
                    id: b.id,
                    name: b.name.clone(),
                    owner: b.owner.clone(),
                    access: b.access_for(user)?,
                    items: b.items.len(),
                })
            })
            .collect();
        list.sort_by(|a, b| {
            (a.access != Access::Owner)
                .cmp(&(b.access != Access::Owner))
                .then_with(|| a.name.cmp(&b.name))
        });
        list
    }

    pub fn create_board(&self, user: &str, name: &str) -> Result<BoardSummary> {
        let name = name.trim();
        validate_document_name(name)?;
        let mut index = self.index.lock().expect("index lock");
        if index
            .boards
            .iter()
            .any(|b| b.owner == user && b.name == name)
        {
            return Err(StoreError::Conflict(format!(
                "you already have a board named \"{name}\""
            )));
        }
        let id = index.next_board_id;
        index.next_board_id += 1;
        index.boards.push(BoardRecord {
            id,
            owner: user.into(),
            name: name.into(),
            shares: BTreeMap::new(),
            items: Vec::new(),
            next_item_id: 1,
        });
        self.persist(&index)?;
        Ok(BoardSummary {
            id,
            name: name.into(),
            owner: user.into(),
            access: Access::Owner,
            items: 0,
        })
    }

    pub fn delete_board(&self, user: &str, id: u64) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::board_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        index.boards.remove(position);
        self.persist(&index)?;
        let dir = self.root.join("blobs").join(format!("board-{id}"));
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn list_board_shares(&self, user: &str, id: u64) -> Result<Vec<Share>> {
        let index = self.index.lock().expect("index lock");
        let (position, access) = Self::board_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        Ok(index.boards[position]
            .shares
            .iter()
            .map(|(user, role)| Share {
                user: user.clone(),
                role: *role,
            })
            .collect())
    }

    pub fn set_board_share(&self, user: &str, id: u64, target: &str, role: Role) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::board_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        if !Self::user_exists(&index, target) {
            return Err(StoreError::Invalid(format!("no user named \"{target}\"")));
        }
        if index.boards[position].owner == target {
            return Err(StoreError::Invalid(
                "the owner already has full access".into(),
            ));
        }
        index.boards[position].shares.insert(target.into(), role);
        self.persist(&index)
    }

    pub fn remove_board_share(&self, user: &str, id: u64, target: &str) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (position, access) = Self::board_index(&index, user, id)?;
        if access != Access::Owner {
            return Err(StoreError::Forbidden);
        }
        if index.boards[position].shares.remove(target).is_none() {
            return Err(StoreError::NotFound);
        }
        self.persist(&index)
    }

    pub fn list_board_items(&self, user: &str, id: u64) -> Result<Vec<BoardItem>> {
        let index = self.index.lock().expect("index lock");
        let (position, _) = Self::board_index(&index, user, id)?;
        Ok(index.boards[position].items.clone())
    }

    /// Pins an item to a board. An `image` is its PNG `blob`; a `note`
    /// or `prompt` is its `text`. With no position given, items are laid
    /// out left to right in a row of 5, 320 units apart, so a board fills
    /// in reading order until someone moves things. Needs edit access.
    #[allow(clippy::too_many_arguments)]
    pub fn add_board_item(
        &self,
        user: &str,
        id: u64,
        kind: &str,
        name: &str,
        text: &str,
        position: Option<(f64, f64)>,
        size: Option<(f64, f64)>,
        blob: Option<&[u8]>,
    ) -> Result<BoardItem> {
        if !BOARD_ITEM_KINDS.contains(&kind) {
            return Err(StoreError::Invalid(format!(
                "a board item is one of {}",
                BOARD_ITEM_KINDS.join(", ")
            )));
        }
        if (kind == "image") != blob.is_some() {
            return Err(StoreError::Invalid(
                "an image is its PNG bytes; a note or prompt is its text".into(),
            ));
        }
        if blob.is_some_and(<[u8]>::is_empty) {
            return Err(StoreError::Invalid(
                "an empty image cannot be pinned".into(),
            ));
        }
        if kind != "image" && text.trim().is_empty() {
            return Err(StoreError::Invalid(
                "a note or prompt needs some text".into(),
            ));
        }
        if text.chars().count() > 4000 {
            return Err(StoreError::Invalid(
                "an item's text is up to 4000 characters".into(),
            ));
        }
        let name = name.trim();
        if name.chars().count() > 200 || name.chars().any(char::is_control) {
            return Err(StoreError::Invalid(
                "an item name is up to 200 characters".into(),
            ));
        }
        let mut index = self.index.lock().expect("index lock");
        let (at, access) = Self::board_index(&index, user, id)?;
        if !access.can_edit() {
            return Err(StoreError::Forbidden);
        }
        let board = &mut index.boards[at];
        let item_id = board.next_item_id;
        board.next_item_id += 1;
        let slot = board.items.len() as f64;
        let (x, y) = position.unwrap_or(((slot % 5.0) * 320.0, (slot / 5.0).floor() * 320.0));
        let (w, h) = size.unwrap_or((300.0, 300.0));
        if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite())
            || w <= 0.0
            || h <= 0.0
        {
            return Err(StoreError::Invalid(
                "an item needs a finite position and a positive size".into(),
            ));
        }
        if let Some(bytes) = blob {
            let path = self.board_blob_path(id, item_id);
            fs::create_dir_all(path.parent().expect("blob dir"))?;
            write_atomic(&path, bytes)?;
        }
        let item = BoardItem {
            id: item_id,
            kind: kind.into(),
            name: if name.is_empty() {
                format!("{kind} {item_id}")
            } else {
                name.into()
            },
            text: text.trim().into(),
            x,
            y,
            w,
            h,
            bytes: blob.map_or(0, |b| b.len() as u64),
            added_by: user.into(),
            added_at: now(),
        };
        board.items.push(item.clone());
        self.persist(&index)?;
        Ok(item)
    }

    /// Moves, resizes, retitles or rewrites an item. Needs edit access.
    pub fn update_board_item(
        &self,
        user: &str,
        id: u64,
        item_id: u64,
        patch: BoardItemPatch,
    ) -> Result<BoardItem> {
        let mut index = self.index.lock().expect("index lock");
        let (at, access) = Self::board_index(&index, user, id)?;
        if !access.can_edit() {
            return Err(StoreError::Forbidden);
        }
        let item = index.boards[at]
            .items
            .iter_mut()
            .find(|i| i.id == item_id)
            .ok_or(StoreError::NotFound)?;
        let (x, y) = (patch.x.unwrap_or(item.x), patch.y.unwrap_or(item.y));
        let (w, h) = (patch.w.unwrap_or(item.w), patch.h.unwrap_or(item.h));
        if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite())
            || w <= 0.0
            || h <= 0.0
        {
            return Err(StoreError::Invalid(
                "an item needs a finite position and a positive size".into(),
            ));
        }
        if let Some(text) = &patch.text {
            if item.kind != "image" && text.trim().is_empty() {
                return Err(StoreError::Invalid(
                    "a note or prompt needs some text".into(),
                ));
            }
            if text.chars().count() > 4000 {
                return Err(StoreError::Invalid(
                    "an item's text is up to 4000 characters".into(),
                ));
            }
        }
        if let Some(name) = &patch.name {
            if name.trim().is_empty()
                || name.chars().count() > 200
                || name.chars().any(char::is_control)
            {
                return Err(StoreError::Invalid(
                    "an item name is 1 to 200 characters".into(),
                ));
            }
        }
        item.x = x;
        item.y = y;
        item.w = w;
        item.h = h;
        if let Some(text) = patch.text {
            item.text = text.trim().to_string();
        }
        if let Some(name) = patch.name {
            item.name = name.trim().to_string();
        }
        let item = item.clone();
        self.persist(&index)?;
        Ok(item)
    }

    pub fn get_board_item_blob(&self, user: &str, id: u64, item_id: u64) -> Result<Vec<u8>> {
        let index = self.index.lock().expect("index lock");
        let (at, _) = Self::board_index(&index, user, id)?;
        let item = index.boards[at]
            .items
            .iter()
            .find(|i| i.id == item_id)
            .ok_or(StoreError::NotFound)?;
        if item.kind != "image" {
            return Err(StoreError::NotFound);
        }
        Ok(fs::read(self.board_blob_path(id, item_id))?)
    }

    pub fn delete_board_item(&self, user: &str, id: u64, item_id: u64) -> Result<()> {
        let mut index = self.index.lock().expect("index lock");
        let (at, access) = Self::board_index(&index, user, id)?;
        if !access.can_edit() {
            return Err(StoreError::Forbidden);
        }
        let board = &mut index.boards[at];
        let pos = board
            .items
            .iter()
            .position(|i| i.id == item_id)
            .ok_or(StoreError::NotFound)?;
        board.items.remove(pos);
        self.persist(&index)?;
        let path = self.board_blob_path(id, item_id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
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
    fn libraries_hold_json_assets_and_graphics_under_the_same_roles() {
        let (dir, store, _) = fresh();
        store.create_user("ana").unwrap();
        let lib = store.create_library("owner", " Brand ").unwrap();
        assert_eq!(lib.name, "Brand");
        assert!(matches!(
            store.create_library("owner", "Brand"),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.create_library("owner", ""),
            Err(StoreError::Invalid(_))
        ));
        let red = store
            .add_asset(
                "owner",
                lib.id,
                "Red",
                "color",
                serde_json::json!({ "hex": "#ff0000" }),
                None,
            )
            .unwrap();
        assert_eq!(red.id, 1);
        let logo = store
            .add_asset(
                "owner",
                lib.id,
                "Logo",
                "graphic",
                serde_json::Value::Null,
                Some(b"PNG..."),
            )
            .unwrap();
        assert_eq!(logo.bytes, 6);
        assert_eq!(
            store.get_asset_blob("owner", lib.id, logo.id).unwrap(),
            b"PNG..."
        );
        assert_eq!(
            store.get_asset_blob("owner", lib.id, red.id),
            Err(StoreError::NotFound)
        );
        assert!(matches!(
            store.add_asset("owner", lib.id, "x", "brush", serde_json::Value::Null, None),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.add_asset(
                "owner",
                lib.id,
                "x",
                "graphic",
                serde_json::Value::Null,
                None
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.add_asset(
                "owner",
                lib.id,
                "x",
                "color",
                serde_json::Value::Null,
                Some(b"png")
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.add_asset(
                "owner",
                lib.id,
                "x",
                "graphic",
                serde_json::Value::Null,
                Some(b"")
            ),
            Err(StoreError::Invalid(_))
        ));
        // Re-adding a name within a kind replaces it; across kinds it does not.
        let red2 = store
            .add_asset(
                "owner",
                lib.id,
                "Red",
                "color",
                serde_json::json!({ "hex": "#ee0000" }),
                None,
            )
            .unwrap();
        store
            .add_asset(
                "owner",
                lib.id,
                "Red",
                "gradient",
                serde_json::json!({ "startColor": [255, 0, 0, 255], "endColor": [0, 0, 0, 255] }),
                None,
            )
            .unwrap();
        let assets = store.list_assets("owner", lib.id).unwrap();
        assert_eq!(assets.len(), 3);
        assert_eq!(assets.iter().filter(|a| a.name == "Red").count(), 2);
        assert_eq!(
            assets.iter().find(|a| a.kind == "color").unwrap().id,
            red2.id
        );

        // Shares work exactly as for documents.
        assert_eq!(store.list_assets("ana", lib.id), Err(StoreError::NotFound));
        store
            .set_library_share("owner", lib.id, "ana", Role::View)
            .unwrap();
        assert_eq!(store.list_assets("ana", lib.id).unwrap().len(), 3);
        assert_eq!(
            store.add_asset(
                "ana",
                lib.id,
                "Blue",
                "color",
                serde_json::json!({ "hex": "#0000ff" }),
                None
            ),
            Err(StoreError::Forbidden)
        );
        assert_eq!(
            store.delete_asset("ana", lib.id, logo.id),
            Err(StoreError::Forbidden)
        );
        store
            .set_library_share("owner", lib.id, "ana", Role::Edit)
            .unwrap();
        store
            .add_asset(
                "ana",
                lib.id,
                "Blue",
                "color",
                serde_json::json!({ "hex": "#0000ff" }),
                None,
            )
            .unwrap();
        assert_eq!(store.list_libraries("ana")[0].access, Access::Edit);
        assert_eq!(store.list_libraries("ana")[0].assets, 4);
        assert_eq!(
            store.delete_library("ana", lib.id),
            Err(StoreError::Forbidden)
        );
        assert_eq!(
            store.list_library_shares("owner", lib.id).unwrap(),
            vec![Share {
                user: "ana".into(),
                role: Role::Edit
            }]
        );
        store.remove_library_share("owner", lib.id, "ana").unwrap();
        assert_eq!(store.list_assets("ana", lib.id), Err(StoreError::NotFound));

        // Deleting an asset removes its blob; reopening keeps the rest.
        store.delete_asset("owner", lib.id, logo.id).unwrap();
        assert!(!dir
            .path()
            .join("blobs")
            .join(format!("lib-{}", lib.id))
            .join(logo.id.to_string())
            .exists());
        assert_eq!(
            store.delete_asset("owner", lib.id, logo.id),
            Err(StoreError::NotFound)
        );
        drop(store);
        let (reopened, _) = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.list_assets("owner", lib.id).unwrap().len(), 3);
        reopened.delete_library("owner", lib.id).unwrap();
        assert!(reopened.list_libraries("owner").is_empty());
        assert!(!dir
            .path()
            .join("blobs")
            .join(format!("lib-{}", lib.id))
            .exists());
    }

    #[test]
    fn boards_pin_images_notes_and_prompts_in_reading_order() {
        let (dir, store, _) = fresh();
        store.create_user("ana").unwrap();
        let board = store.create_board("owner", " Autumn campaign ").unwrap();
        assert_eq!(board.name, "Autumn campaign");
        assert!(matches!(
            store.create_board("owner", "Autumn campaign"),
            Err(StoreError::Conflict(_))
        ));
        let note = store
            .add_board_item(
                "owner",
                board.id,
                "note",
                "",
                " Warm palette ",
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            (note.id, note.x, note.y, note.w, note.h),
            (1, 0.0, 0.0, 300.0, 300.0)
        );
        assert_eq!(note.name, "note 1");
        assert_eq!(note.text, "Warm palette");
        let image = store
            .add_board_item(
                "owner",
                board.id,
                "image",
                "Hero",
                "",
                None,
                None,
                Some(b"PNG"),
            )
            .unwrap();
        assert_eq!((image.x, image.y, image.bytes), (320.0, 0.0, 3));
        assert_eq!(
            store
                .get_board_item_blob("owner", board.id, image.id)
                .unwrap(),
            b"PNG"
        );
        assert_eq!(
            store.get_board_item_blob("owner", board.id, note.id),
            Err(StoreError::NotFound)
        );
        for _ in 0..3 {
            store
                .add_board_item(
                    "owner",
                    board.id,
                    "prompt",
                    "",
                    "a forest lake at dawn",
                    None,
                    None,
                    None,
                )
                .unwrap();
        }
        let sixth = store
            .add_board_item(
                "owner",
                board.id,
                "note",
                "",
                "row two",
                Some((10.0, 20.0)),
                Some((50.0, 60.0)),
                None,
            )
            .unwrap();
        assert_eq!(
            (sixth.x, sixth.y, sixth.w, sixth.h),
            (10.0, 20.0, 50.0, 60.0)
        );
        let seventh = store
            .add_board_item("owner", board.id, "note", "", "auto", None, None, None)
            .unwrap();
        assert_eq!((seventh.x, seventh.y), (320.0, 320.0));
        for bad in [
            store.add_board_item("owner", board.id, "video", "", "x", None, None, None),
            store.add_board_item("owner", board.id, "image", "", "", None, None, None),
            store.add_board_item("owner", board.id, "note", "", "x", None, None, Some(b"png")),
            store.add_board_item("owner", board.id, "image", "", "", None, None, Some(b"")),
            store.add_board_item("owner", board.id, "note", "", "  ", None, None, None),
            store.add_board_item(
                "owner",
                board.id,
                "note",
                "",
                "x",
                Some((f64::NAN, 0.0)),
                None,
                None,
            ),
            store.add_board_item(
                "owner",
                board.id,
                "note",
                "",
                "x",
                None,
                Some((0.0, 10.0)),
                None,
            ),
        ] {
            assert!(matches!(bad, Err(StoreError::Invalid(_))), "{bad:?}");
        }
        // Updates: move, resize, retitle, rewrite -- with the same checks.
        let moved = store
            .update_board_item(
                "owner",
                board.id,
                note.id,
                BoardItemPatch {
                    x: Some(5.0),
                    text: Some("Warm, muted palette".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (moved.x, moved.y, moved.text.as_str()),
            (5.0, 0.0, "Warm, muted palette")
        );
        assert!(matches!(
            store.update_board_item(
                "owner",
                board.id,
                note.id,
                BoardItemPatch {
                    w: Some(0.0),
                    ..Default::default()
                }
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.update_board_item(
                "owner",
                board.id,
                note.id,
                BoardItemPatch {
                    text: Some(" ".into()),
                    ..Default::default()
                }
            ),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(
            store.update_board_item("owner", board.id, 99, BoardItemPatch::default()),
            Err(StoreError::NotFound)
        );
        // Roles as everywhere else.
        assert_eq!(
            store.list_board_items("ana", board.id),
            Err(StoreError::NotFound)
        );
        store
            .set_board_share("owner", board.id, "ana", Role::View)
            .unwrap();
        assert_eq!(store.list_board_items("ana", board.id).unwrap().len(), 7);
        assert_eq!(
            store.add_board_item("ana", board.id, "note", "", "x", None, None, None),
            Err(StoreError::Forbidden)
        );
        assert_eq!(
            store.update_board_item("ana", board.id, note.id, BoardItemPatch::default()),
            Err(StoreError::Forbidden)
        );
        assert_eq!(
            store.delete_board_item("ana", board.id, note.id),
            Err(StoreError::Forbidden)
        );
        store
            .set_board_share("owner", board.id, "ana", Role::Edit)
            .unwrap();
        store
            .add_board_item("ana", board.id, "note", "", "from ana", None, None, None)
            .unwrap();
        assert_eq!(store.list_boards("ana")[0].items, 8);
        assert_eq!(store.list_boards("ana")[0].access, Access::Edit);
        assert_eq!(
            store.delete_board("ana", board.id),
            Err(StoreError::Forbidden)
        );
        assert_eq!(
            store.list_board_shares("owner", board.id).unwrap(),
            vec![Share {
                user: "ana".into(),
                role: Role::Edit
            }]
        );
        store.remove_board_share("owner", board.id, "ana").unwrap();
        assert_eq!(
            store.list_board_items("ana", board.id),
            Err(StoreError::NotFound)
        );
        // Deleting an image item removes its blob; reopening keeps the rest.
        store
            .delete_board_item("owner", board.id, image.id)
            .unwrap();
        assert!(!dir
            .path()
            .join("blobs")
            .join(format!("board-{}", board.id))
            .join(image.id.to_string())
            .exists());
        drop(store);
        let (reopened, _) = Store::open(dir.path()).unwrap();
        assert_eq!(
            reopened.list_board_items("owner", board.id).unwrap().len(),
            7
        );
        reopened.delete_board("owner", board.id).unwrap();
        assert!(reopened.list_boards("owner").is_empty());
        assert!(!dir
            .path()
            .join("blobs")
            .join(format!("board-{}", board.id))
            .exists());
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
