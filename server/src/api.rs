//! The HTTP surface: the contract `src/App.tsx` speaks (`GET /documents`,
//! `PUT`/`GET /documents/{name}`) plus everything Invite to Edit and
//! Share for Review need. Every handler is a thin translation from HTTP
//! to a `Store` call; the rules live in `store.rs`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::{header, request::Parts, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};

use crate::store::{BoardItemPatch, Principal, Role, Store, StoreError};

/// What every handler shares: the store, and the data directory the
/// font cache lives under.
pub struct App {
    pub store: Store,
    pub data_dir: std::path::PathBuf,
}

impl std::ops::Deref for App {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}

pub type Shared = Arc<App>;

/// The largest document the server accepts: the same 64 MB the desktop
/// app's own `check_canvas_bytes` allows.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

pub fn router(store: Store, data_dir: std::path::PathBuf) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::PUT,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any);
    Router::new()
        .route("/health", get(health))
        .route("/me", get(me))
        .route("/users", get(list_users).post(create_user))
        .route("/users/{name}/token", post(reset_token))
        .route("/documents", get(list_documents))
        .route(
            "/documents/{name}",
            get(get_document).put(put_document).delete(delete_document),
        )
        .route("/documents/{name}/versions", get(list_versions))
        .route("/documents/{name}/versions/{version}", get(get_version))
        .route("/documents/{name}/shares", get(list_shares))
        .route(
            "/documents/{name}/shares/{user}",
            axum::routing::put(set_share).delete(remove_share),
        )
        .route(
            "/documents/{name}/reviews",
            get(list_reviews).post(create_review),
        )
        .route("/libraries", get(list_libraries).post(create_library))
        .route("/libraries/{id}", axum::routing::delete(delete_library))
        .route("/libraries/{id}/shares", get(list_library_shares))
        .route(
            "/libraries/{id}/shares/{user}",
            axum::routing::put(set_library_share).delete(remove_library_share),
        )
        .route("/libraries/{id}/assets", get(list_assets).post(add_asset))
        .route(
            "/libraries/{id}/graphics/{name}",
            axum::routing::put(add_graphic),
        )
        .route(
            "/libraries/{id}/assets/{asset}",
            axum::routing::delete(delete_asset),
        )
        .route("/libraries/{id}/assets/{asset}/blob", get(get_asset_blob))
        .route("/boards", get(list_boards).post(create_board))
        .route("/boards/{id}", get(get_board).delete(delete_board))
        .route("/boards/{id}/shares", get(list_board_shares))
        .route(
            "/boards/{id}/shares/{user}",
            axum::routing::put(set_board_share).delete(remove_board_share),
        )
        .route("/boards/{id}/items", post(add_board_item))
        .route(
            "/boards/{id}/images/{name}",
            axum::routing::put(add_board_image),
        )
        .route(
            "/boards/{id}/items/{item}",
            axum::routing::patch(update_board_item).delete(delete_board_item),
        )
        .route("/boards/{id}/items/{item}/blob", get(get_board_item_blob))
        .route("/fonts", get(list_fonts))
        .route("/fonts/{family}/file", get(font_file))
        .route("/reviews/{id}", get(get_review))
        .route("/reviews/{id}/document", get(get_review_document))
        .route("/reviews/{id}/comments", post(add_comment))
        .route(
            "/reviews/{id}/comments/{comment}/resolved",
            axum::routing::put(set_resolved),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_DOCUMENT_BYTES))
        .layer(cors)
        .with_state(Arc::new(App { store, data_dir }))
}

/// An error the client can read: `{ "error": "..." }` with the status
/// the store's refusal maps to.
pub struct ApiError(StatusCode, String);

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError(StatusCode::NOT_FOUND, "not found".into()),
            StoreError::Forbidden => ApiError(StatusCode::FORBIDDEN, "not allowed".into()),
            StoreError::Conflict(m) => ApiError(StatusCode::CONFLICT, m),
            StoreError::Invalid(m) => ApiError(StatusCode::BAD_REQUEST, m),
            StoreError::Io(m) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, m),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

/// A signed-in user, from `Authorization: Bearer <token>`.
pub struct AuthUser(String);

/// The admin token specifically.
pub struct AuthAdmin;

fn bearer(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

impl FromRequestParts<Shared> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        store: &Shared,
    ) -> Result<Self, Self::Rejection> {
        match bearer(parts).and_then(|t| store.authenticate(t)) {
            Some(Principal::User(name)) => Ok(AuthUser(name)),
            Some(Principal::Admin) => Err(ApiError(
                StatusCode::FORBIDDEN,
                "the admin token manages users; documents need a user token".into(),
            )),
            None => Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "a valid bearer token is required".into(),
            )),
        }
    }
}

impl FromRequestParts<Shared> for AuthAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        store: &Shared,
    ) -> Result<Self, Self::Rejection> {
        match bearer(parts).and_then(|t| store.authenticate(t)) {
            Some(Principal::Admin) => Ok(AuthAdmin),
            _ => Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "the admin token is required".into(),
            )),
        }
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(
        json!({ "ok": true, "service": "image-editor-server", "version": env!("CARGO_PKG_VERSION") }),
    )
}

async fn me(AuthUser(user): AuthUser) -> Json<serde_json::Value> {
    Json(json!({ "user": user }))
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
}

async fn list_users(_: AuthAdmin, State(store): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({ "users": store.list_users() }))
}

async fn create_user(
    _: AuthAdmin,
    State(store): State<Shared>,
    Json(body): Json<NewUser>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let token = store.create_user(&body.name)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "user": body.name, "token": token })),
    ))
}

async fn reset_token(
    _: AuthAdmin,
    State(store): State<Shared>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = store.reset_user_token(&name)?;
    Ok(Json(json!({ "user": name, "token": token })))
}

async fn list_documents(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
) -> Json<serde_json::Value> {
    let details = store.list_documents(&user);
    let names: Vec<&str> = details.iter().map(|d| d.name.as_str()).collect();
    Json(json!({ "documents": names, "details": details }))
}

fn octets(bytes: Vec<u8>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        )],
        bytes,
    )
        .into_response()
}

async fn get_document(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    Ok(octets(store.get_document(&user, &name, None)?))
}

async fn put_document(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(name): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let version = store.put_document(&user, &name, &body)?;
    Ok(Json(json!({ "name": name, "version": version })))
}

async fn delete_document(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    store.delete_document(&user, &name)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_versions(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        json!({ "versions": store.list_versions(&user, &name)? }),
    ))
}

async fn get_version(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((name, version)): Path<(String, u32)>,
) -> Result<Response, ApiError> {
    Ok(octets(store.get_document(&user, &name, Some(version))?))
}

async fn list_shares(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(json!({ "shares": store.list_shares(&user, &name)? })))
}

#[derive(Deserialize)]
struct ShareBody {
    role: Role,
}

async fn set_share(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((name, target)): Path<(String, String)>,
    Json(body): Json<ShareBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    store.set_share(&user, &name, &target, body.role)?;
    Ok(Json(json!({ "user": target, "role": body.role })))
}

async fn remove_share(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((name, target)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    store.remove_share(&user, &name, &target)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct NewReview {
    #[serde(default)]
    title: String,
}

async fn create_review(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(name): Path<String>,
    body: Option<Json<NewReview>>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let title = body.map(|Json(b)| b.title).unwrap_or_default();
    let review = store.create_review(&user, &name, &title)?;
    let path = format!("/reviews/{}", review.id);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "review": review, "path": path })),
    ))
}

async fn list_reviews(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        json!({ "reviews": store.list_reviews(&user, &name)? }),
    ))
}

async fn get_review(
    State(store): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(json!({ "review": store.get_review(&id)? })))
}

async fn get_review_document(
    State(store): State<Shared>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(octets(store.get_review_document(&id)?))
}

#[derive(Deserialize)]
struct NewComment {
    author: String,
    text: String,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    parent: Option<u64>,
}

async fn add_comment(
    State(store): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<NewComment>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let comment = store.add_comment(&id, &body.author, &body.text, body.x, body.y, body.parent)?;
    Ok((StatusCode::CREATED, Json(json!({ "comment": comment }))))
}

async fn list_libraries(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
) -> Json<serde_json::Value> {
    Json(json!({ "libraries": store.list_libraries(&user) }))
}

#[derive(Deserialize)]
struct NewLibrary {
    name: String,
}

async fn create_library(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Json(body): Json<NewLibrary>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let library = store.create_library(&user, &body.name)?;
    Ok((StatusCode::CREATED, Json(json!({ "library": library }))))
}

async fn delete_library(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(id): Path<u64>,
) -> Result<StatusCode, ApiError> {
    store.delete_library(&user, id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_library_shares(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        json!({ "shares": store.list_library_shares(&user, id)? }),
    ))
}

async fn set_library_share(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((id, target)): Path<(u64, String)>,
    Json(body): Json<ShareBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    store.set_library_share(&user, id, &target, body.role)?;
    Ok(Json(json!({ "user": target, "role": body.role })))
}

async fn remove_library_share(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((id, target)): Path<(u64, String)>,
) -> Result<StatusCode, ApiError> {
    store.remove_library_share(&user, id, &target)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_assets(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(json!({ "assets": store.list_assets(&user, id)? })))
}

#[derive(Deserialize)]
struct NewAsset {
    name: String,
    kind: String,
    #[serde(default)]
    data: serde_json::Value,
}

async fn add_asset(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path(id): Path<u64>,
    Json(body): Json<NewAsset>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let asset = store.add_asset(&user, id, &body.name, &body.kind, body.data, None)?;
    Ok((StatusCode::CREATED, Json(json!({ "asset": asset }))))
}

async fn add_graphic(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((id, name)): Path<(u64, String)>,
    body: Bytes,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let asset = store.add_asset(
        &user,
        id,
        &name,
        "graphic",
        serde_json::Value::Null,
        Some(&body),
    )?;
    Ok((StatusCode::CREATED, Json(json!({ "asset": asset }))))
}

async fn get_asset_blob(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((id, asset)): Path<(u64, u64)>,
) -> Result<Response, ApiError> {
    Ok(octets(store.get_asset_blob(&user, id, asset)?))
}

async fn delete_asset(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((id, asset)): Path<(u64, u64)>,
) -> Result<StatusCode, ApiError> {
    store.delete_asset(&user, id, asset)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_boards(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
) -> Json<serde_json::Value> {
    Json(json!({ "boards": app.list_boards(&user) }))
}

#[derive(Deserialize)]
struct NewBoard {
    name: String,
}

async fn create_board(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Json(body): Json<NewBoard>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let board = app.create_board(&user, &body.name)?;
    Ok((StatusCode::CREATED, Json(json!({ "board": board }))))
}

async fn get_board(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(json!({ "items": app.list_board_items(&user, id)? })))
}

async fn delete_board(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path(id): Path<u64>,
) -> Result<StatusCode, ApiError> {
    app.delete_board(&user, id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_board_shares(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(json!({ "shares": app.list_board_shares(&user, id)? })))
}

async fn set_board_share(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path((id, target)): Path<(u64, String)>,
    Json(body): Json<ShareBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    app.set_board_share(&user, id, &target, body.role)?;
    Ok(Json(json!({ "user": target, "role": body.role })))
}

async fn remove_board_share(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path((id, target)): Path<(u64, String)>,
) -> Result<StatusCode, ApiError> {
    app.remove_board_share(&user, id, &target)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NewBoardItem {
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    text: String,
    x: Option<f64>,
    y: Option<f64>,
    w: Option<f64>,
    h: Option<f64>,
}

async fn add_board_item(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path(id): Path<u64>,
    Json(body): Json<NewBoardItem>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let position = body.x.zip(body.y);
    let size = body.w.zip(body.h);
    let item = app.add_board_item(
        &user, id, &body.kind, &body.name, &body.text, position, size, None,
    )?;
    Ok((StatusCode::CREATED, Json(json!({ "item": item }))))
}

async fn add_board_image(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path((id, name)): Path<(u64, String)>,
    body: Bytes,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let item = app.add_board_item(&user, id, "image", &name, "", None, None, Some(&body))?;
    Ok((StatusCode::CREATED, Json(json!({ "item": item }))))
}

async fn update_board_item(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path((id, item)): Path<(u64, u64)>,
    Json(patch): Json<BoardItemPatch>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let item = app.update_board_item(&user, id, item, patch)?;
    Ok(Json(json!({ "item": item })))
}

async fn get_board_item_blob(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path((id, item)): Path<(u64, u64)>,
) -> Result<Response, ApiError> {
    Ok(octets(app.get_board_item_blob(&user, id, item)?))
}

async fn delete_board_item(
    AuthUser(user): AuthUser,
    State(app): State<Shared>,
    Path((id, item)): Path<(u64, u64)>,
) -> Result<StatusCode, ApiError> {
    app.delete_board_item(&user, id, item)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_fonts(AuthUser(_): AuthUser, State(app): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({ "fonts": crate::fonts::list(&app.data_dir) }))
}

#[derive(Deserialize)]
struct FontQuery {
    #[serde(default = "regular")]
    weight: u16,
    #[serde(default)]
    italic: bool,
}

fn regular() -> u16 {
    400
}

async fn font_file(
    AuthUser(_): AuthUser,
    State(app): State<Shared>,
    Path(family): Path<String>,
    axum::extract::Query(query): axum::extract::Query<FontQuery>,
) -> Result<Response, ApiError> {
    let data_dir = app.data_dir.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        crate::fonts::font_file(&data_dir, &family, query.weight, query.italic)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok((
        [(header::CONTENT_TYPE, HeaderValue::from_static("font/ttf"))],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize, Serialize)]
struct Resolved {
    resolved: bool,
}

async fn set_resolved(
    AuthUser(user): AuthUser,
    State(store): State<Shared>,
    Path((id, comment)): Path<(String, u64)>,
    Json(body): Json<Resolved>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let comment = store.set_comment_resolved(&user, &id, comment, body.resolved)?;
    Ok(Json(json!({ "comment": comment })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    struct TestApp {
        router: Router,
        admin: String,
        owner: String,
        _dir: tempfile::TempDir,
    }

    fn app() -> TestApp {
        let dir = tempfile::tempdir().unwrap();
        let (store, tokens) = Store::open(dir.path()).unwrap();
        let tokens = tokens.unwrap();
        TestApp {
            router: router(store, dir.path().to_path_buf()),
            admin: tokens.admin_token,
            owner: tokens.owner_token,
            _dir: dir,
        }
    }

    async fn call(
        router: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<(&str, Vec<u8>)>,
    ) -> (StatusCode, Vec<u8>) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let request = match body {
            Some((content_type, bytes)) => request
                .header("Content-Type", content_type)
                .body(Body::from(bytes))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    fn json(bytes: &[u8]) -> serde_json::Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[tokio::test]
    async fn the_client_contract_round_trips_a_document() {
        let app = app();
        let (status, body) = call(&app.router, "GET", "/health", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&body)["ok"], true);

        // No token: 401. Admin token on a document route: 403.
        let (status, _) = call(&app.router, "GET", "/documents", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = call(&app.router, "GET", "/documents", Some(&app.admin), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, body) = call(&app.router, "GET", "/documents", Some(&app.owner), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&body)["documents"], json!([]));

        // Exactly what App.tsx's saveToCloud sends.
        let (status, body) = call(
            &app.router,
            "PUT",
            "/documents/My%20poster",
            Some(&app.owner),
            Some(("application/octet-stream", b"IMGPROJ v1".to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&body)["version"], 1);
        let (status, body) = call(
            &app.router,
            "GET",
            "/documents/My%20poster",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"IMGPROJ v1");
        // And what refreshCloudDocuments reads.
        let (_, body) = call(&app.router, "GET", "/documents", Some(&app.owner), None).await;
        let body = json(&body);
        assert_eq!(body["documents"], json!(["My poster"]));
        assert_eq!(body["details"][0]["access"], "owner");
        assert_eq!(body["details"][0]["bytes"], 10);

        let (status, _) = call(
            &app.router,
            "GET",
            "/documents/nope",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, body) = call(
            &app.router,
            "PUT",
            "/documents/empty",
            Some(&app.owner),
            Some(("application/octet-stream", Vec::new())),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json(&body)["error"].as_str().unwrap().contains("empty"));

        let (status, body) = call(
            &app.router,
            "GET",
            "/documents/My%20poster/versions",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&body)["versions"][0]["version"], 1);
        let (status, body) = call(
            &app.router,
            "GET",
            "/documents/My%20poster/versions/1",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"IMGPROJ v1");
        let (status, _) = call(
            &app.router,
            "DELETE",
            "/documents/My%20poster",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(
            &app.router,
            "GET",
            "/documents/My%20poster",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invite_to_edit_and_share_for_review_over_http() {
        let app = app();
        // Users are the admin's to create; the token comes back once.
        let (status, _) = call(
            &app.router,
            "POST",
            "/users",
            Some(&app.owner),
            Some(("application/json", br#"{"name":"ana"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = call(
            &app.router,
            "POST",
            "/users",
            Some(&app.admin),
            Some(("application/json", br#"{"name":"ana"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let ana = json(&body)["token"].as_str().unwrap().to_string();
        let (status, _) = call(
            &app.router,
            "POST",
            "/users",
            Some(&app.admin),
            Some(("application/json", br#"{"name":"ana"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (_, body) = call(&app.router, "GET", "/users", Some(&app.admin), None).await;
        assert_eq!(json(&body)["users"], json!(["owner", "ana"]));

        call(
            &app.router,
            "PUT",
            "/documents/poster",
            Some(&app.owner),
            Some(("application/octet-stream", b"v1".to_vec())),
        )
        .await;
        let (status, _) = call(
            &app.router,
            "GET",
            "/documents/owner%2Fposter",
            Some(&ana),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Invite to Edit.
        let (status, body) = call(
            &app.router,
            "PUT",
            "/documents/poster/shares/ana",
            Some(&app.owner),
            Some(("application/json", br#"{"role":"view"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&body)["role"], "view");
        let (status, body) = call(
            &app.router,
            "GET",
            "/documents/owner%2Fposter",
            Some(&ana),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"v1");
        let (status, _) = call(
            &app.router,
            "PUT",
            "/documents/owner%2Fposter",
            Some(&ana),
            Some(("application/octet-stream", b"v2".to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        call(
            &app.router,
            "PUT",
            "/documents/poster/shares/ana",
            Some(&app.owner),
            Some(("application/json", br#"{"role":"edit"}"#.to_vec())),
        )
        .await;
        let (status, _) = call(
            &app.router,
            "PUT",
            "/documents/owner%2Fposter",
            Some(&ana),
            Some(("application/octet-stream", b"v2".to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = call(&app.router, "GET", "/documents", Some(&ana), None).await;
        assert_eq!(json(&body)["documents"], json!(["owner/poster"]));
        let (status, body) = call(
            &app.router,
            "GET",
            "/documents/poster/shares",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json(&body)["shares"],
            json!([{ "user": "ana", "role": "edit" }])
        );
        let (status, _) = call(
            &app.router,
            "PUT",
            "/documents/poster/shares/nobody",
            Some(&app.owner),
            Some(("application/json", br#"{"role":"edit"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app.router,
            "PUT",
            "/documents/poster/shares/ana",
            Some(&app.owner),
            Some(("application/json", br#"{"role":"boss"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        // Share for Review: the link works with no token at all.
        let (status, body) = call(
            &app.router,
            "POST",
            "/documents/poster/reviews",
            Some(&app.owner),
            Some(("application/json", br#"{"title":"Round 1"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let body = json(&body);
        let id = body["review"]["id"].as_str().unwrap().to_string();
        assert_eq!(body["path"], format!("/reviews/{id}"));
        assert_eq!(body["review"]["version"], 2);
        let (status, body) = call(
            &app.router,
            "GET",
            &format!("/reviews/{id}/document"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"v2");
        let (status, body) = call(
            &app.router,
            "POST",
            &format!("/reviews/{id}/comments"),
            None,
            Some((
                "application/json",
                br#"{"author":"Client","text":"Logo bigger","x":0.25,"y":0.75}"#.to_vec(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(json(&body)["comment"]["id"], 1);
        let (status, _) = call(
            &app.router,
            "POST",
            &format!("/reviews/{id}/comments"),
            None,
            Some((
                "application/json",
                br#"{"author":"Client","text":"","x":0.25,"y":0.75}"#.to_vec(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app.router,
            "PUT",
            &format!("/reviews/{id}/comments/1/resolved"),
            None,
            Some(("application/json", br#"{"resolved":true}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = call(
            &app.router,
            "PUT",
            &format!("/reviews/{id}/comments/1/resolved"),
            Some(&ana),
            Some(("application/json", br#"{"resolved":true}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&body)["comment"]["resolved"], true);
        let (status, body) = call(&app.router, "GET", &format!("/reviews/{id}"), None, None).await;
        assert_eq!(status, StatusCode::OK);
        let review = json(&body);
        assert_eq!(review["review"]["title"], "Round 1");
        assert_eq!(review["review"]["comments"][0]["resolved"], true);
        let (_, body) = call(
            &app.router,
            "GET",
            "/documents/poster/reviews",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(json(&body)["reviews"].as_array().unwrap().len(), 1);
        let (status, _) = call(&app.router, "GET", "/reviews/0000/document", None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Removing the share closes the door again; the review link still works.
        let (status, _) = call(
            &app.router,
            "DELETE",
            "/documents/poster/shares/ana",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(
            &app.router,
            "GET",
            "/documents/owner%2Fposter",
            Some(&ana),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&app.router, "GET", &format!("/reviews/{id}"), None, None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn libraries_over_http() {
        let app = app();
        let (status, body) = call(
            &app.router,
            "POST",
            "/libraries",
            Some(&app.owner),
            Some(("application/json", br#"{"name":"Brand"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = json(&body)["library"]["id"].as_u64().unwrap();
        let (status, body) = call(
            &app.router,
            "POST",
            &format!("/libraries/{id}/assets"),
            Some(&app.owner),
            Some((
                "application/json",
                br##"{"name":"Red","kind":"color","data":{"hex":"#ff0000"}}"##.to_vec(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(json(&body)["asset"]["data"]["hex"], "#ff0000");
        let (status, body) = call(
            &app.router,
            "PUT",
            &format!("/libraries/{id}/graphics/Logo"),
            Some(&app.owner),
            Some(("application/octet-stream", b"PNGBYTES".to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let logo = json(&body)["asset"]["id"].as_u64().unwrap();
        let (status, body) = call(
            &app.router,
            "GET",
            &format!("/libraries/{id}/assets/{logo}/blob"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"PNGBYTES");
        let (_, body) = call(&app.router, "GET", "/libraries", Some(&app.owner), None).await;
        assert_eq!(json(&body)["libraries"][0]["assets"], 2);
        let (status, _) = call(
            &app.router,
            "POST",
            &format!("/libraries/{id}/assets"),
            Some(&app.owner),
            Some((
                "application/json",
                br#"{"name":"x","kind":"brush"}"#.to_vec(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app.router,
            "DELETE",
            &format!("/libraries/{id}/assets/{logo}"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = call(
            &app.router,
            "GET",
            &format!("/libraries/{id}/assets"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(json(&body)["assets"].as_array().unwrap().len(), 1);
        let (status, _) = call(
            &app.router,
            "GET",
            "/libraries/999/assets",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            &app.router,
            "DELETE",
            &format!("/libraries/{id}"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn boards_over_http() {
        let app = app();
        let (status, body) = call(
            &app.router,
            "POST",
            "/boards",
            Some(&app.owner),
            Some(("application/json", br#"{"name":"Moodboard"}"#.to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = json(&body)["board"]["id"].as_u64().unwrap();
        let (status, body) = call(
            &app.router,
            "POST",
            &format!("/boards/{id}/items"),
            Some(&app.owner),
            Some((
                "application/json",
                br#"{"kind":"prompt","text":"misty pines"}"#.to_vec(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(json(&body)["item"]["x"], 0.0);
        let (status, body) = call(
            &app.router,
            "PUT",
            &format!("/boards/{id}/images/Hero"),
            Some(&app.owner),
            Some(("application/octet-stream", b"PNGBYTES".to_vec())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let image = json(&body)["item"]["id"].as_u64().unwrap();
        assert_eq!(json(&body)["item"]["x"], 320.0);
        let (status, body) = call(
            &app.router,
            "GET",
            &format!("/boards/{id}/items/{image}/blob"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"PNGBYTES");
        let (status, body) = call(
            &app.router,
            "PATCH",
            &format!("/boards/{id}/items/{image}"),
            Some(&app.owner),
            Some((
                "application/json",
                br#"{"x":40.5,"name":"Hero v2"}"#.to_vec(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&body)["item"]["x"], 40.5);
        assert_eq!(json(&body)["item"]["name"], "Hero v2");
        let (_, body) = call(
            &app.router,
            "GET",
            &format!("/boards/{id}"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(json(&body)["items"].as_array().unwrap().len(), 2);
        let (_, body) = call(&app.router, "GET", "/boards", Some(&app.owner), None).await;
        assert_eq!(json(&body)["boards"][0]["items"], 2);
        let (status, _) = call(
            &app.router,
            "POST",
            &format!("/boards/{id}/items"),
            Some(&app.owner),
            Some((
                "application/json",
                br#"{"kind":"video","text":"x"}"#.to_vec(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app.router,
            "DELETE",
            &format!("/boards/{id}/items/{image}"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(
            &app.router,
            "DELETE",
            &format!("/boards/{id}"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(
            &app.router,
            "GET",
            &format!("/boards/{id}"),
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn fonts_over_http() {
        let app = app();
        let (status, _) = call(&app.router, "GET", "/fonts", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = call(&app.router, "GET", "/fonts", Some(&app.owner), None).await;
        assert_eq!(status, StatusCode::OK);
        let fonts = json(&body);
        assert!(fonts["fonts"].as_array().unwrap().len() > 100);
        assert!(fonts["fonts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["family"] == "Open Sans"));
        // A local file is served as it is; an unknown family is 404; a
        // cached catalogue file is served without network.
        let local = app._dir.path().join("fonts").join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("House.ttf"), b"TTF").unwrap();
        let (status, body) = call(
            &app.router,
            "GET",
            "/fonts/House/file",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"TTF");
        let (status, _) = call(
            &app.router,
            "GET",
            "/fonts/Nope/file",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let cache = app._dir.path().join("fonts").join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("lato-700.ttf"), b"LATO").unwrap();
        let (status, body) = call(
            &app.router,
            "GET",
            "/fonts/Lato/file?weight=700",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"LATO");
        let (status, _) = call(
            &app.router,
            "GET",
            "/fonts/Lato/file?weight=450",
            Some(&app.owner),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn cors_lets_the_webview_call_from_its_own_origin() {
        let app = app();
        let request = Request::builder()
            .method("OPTIONS")
            .uri("/documents")
            .header("Origin", "tauri://localhost")
            .header("Access-Control-Request-Method", "PUT")
            .header(
                "Access-Control-Request-Headers",
                "authorization,content-type",
            )
            .body(Body::empty())
            .unwrap();
        let response = app.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        let methods = response.headers()["access-control-allow-methods"]
            .to_str()
            .unwrap()
            .to_string();
        for method in ["GET", "PUT", "POST", "PATCH", "DELETE"] {
            assert!(methods.contains(method), "{methods} lacks {method}");
        }
    }
}
