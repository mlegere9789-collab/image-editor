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

use crate::store::{Principal, Role, Store, StoreError};

pub type Shared = Arc<Store>;

/// The largest document the server accepts: the same 64 MB the desktop
/// app's own `check_canvas_bytes` allows.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

pub fn router(store: Store) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::PUT,
            Method::POST,
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
        .route("/reviews/{id}", get(get_review))
        .route("/reviews/{id}/document", get(get_review_document))
        .route("/reviews/{id}/comments", post(add_comment))
        .route(
            "/reviews/{id}/comments/{comment}/resolved",
            axum::routing::put(set_resolved),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_DOCUMENT_BYTES))
        .layer(cors)
        .with_state(Arc::new(store))
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

    struct App {
        router: Router,
        admin: String,
        owner: String,
        _dir: tempfile::TempDir,
    }

    fn app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let (store, tokens) = Store::open(dir.path()).unwrap();
        let tokens = tokens.unwrap();
        App {
            router: router(store),
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
        assert!(response.headers()["access-control-allow-methods"]
            .to_str()
            .unwrap()
            .contains("PUT"));
    }
}
