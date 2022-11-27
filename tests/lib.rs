use axum::response::IntoResponse;
use sqlx::{sqlite::SqliteArguments, Arguments as _};
use tempfile::NamedTempFile;
use tower::ServiceExt;

type Tx<E = axum_sqlx_tx::Error> = axum_sqlx_tx::Tx<sqlx::Sqlite, E>;

#[tokio::test]
async fn commit_on_success() {
    let (_db, pool, response) = build_app(|mut tx: Tx| async move {
        let (_, name) = insert_user(&mut tx, 1, "huge hackerman").await;
        format!("hello {name}")
    })
    .await;

    assert!(response.status.is_success());
    assert_eq!(response.body, "hello huge hackerman");

    let users: Vec<(i32, String)> = sqlx::query_as("SELECT * FROM users")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(users, vec![(1, "huge hackerman".to_string())]);
}

#[tokio::test]
async fn commit_on_redirection() {
    let (_db, pool, response) = build_app(|mut tx: Tx| async move {
        let (_, _) = insert_user(&mut tx, 1, "john redirect").await;
        http::StatusCode::SEE_OTHER
    })
    .await;

    assert!(response.status.is_redirection());

    let users: Vec<(i32, String)> = sqlx::query_as("SELECT * FROM users")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(users, vec![(1, "john redirect".to_string())]);
}

#[tokio::test]
async fn rollback_on_error() {
    let (_db, pool, response) = build_app(|mut tx: Tx| async move {
        insert_user(&mut tx, 1, "michael oxmaul").await;
        http::StatusCode::BAD_REQUEST
    })
    .await;

    assert!(response.status.is_client_error());
    assert!(response.body.is_empty());

    assert_eq!(get_users(&pool).await, vec![]);
}

#[tokio::test]
async fn explicit_commit() {
    let (_db, pool, response) = build_app(|mut tx: Tx| async move {
        insert_user(&mut tx, 1, "michael oxmaul").await;
        tx.commit().await.unwrap();
        http::StatusCode::BAD_REQUEST
    })
    .await;

    assert!(response.status.is_client_error());
    assert!(response.body.is_empty());

    assert_eq!(
        get_users(&pool).await,
        vec![(1, "michael oxmaul".to_string())]
    );
}

#[tokio::test]
async fn missing_layer() {
    let app = axum::Router::new().route("/", axum::routing::get(|_: Tx| async move {}));
    let response = app
        .oneshot(
            http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_server_error());

    let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
    assert_eq!(body, format!("{}", axum_sqlx_tx::Error::MissingExtension));
}

#[tokio::test]
async fn overlapping_extractors() {
    let (_, _, response) = build_app(|_: Tx, _: Tx| async move {}).await;

    assert!(response.status.is_server_error());
    assert_eq!(
        response.body,
        format!("{}", axum_sqlx_tx::Error::OverlappingExtractors)
    );
}

#[tokio::test]
async fn extractor_error_override() {
    let (_, _, response) = build_app(|_: Tx, _: Tx<MyError>| async move {}).await;

    assert!(response.status.is_client_error());
    assert_eq!(response.body, "internal server error");
}

#[tokio::test]
async fn layer_error_override() {
    let db = NamedTempFile::new().unwrap();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db.path().display()))
        .await
        .unwrap();

    sqlx::query("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY);")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS comments (
            id INT PRIMARY KEY,
            user_id INT,
            FOREIGN KEY (user_id) REFERENCES users(id) DEFERRABLE INITIALLY DEFERRED
        );"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let app = axum::Router::new()
        .route(
            "/",
            axum::routing::get(|mut tx: Tx| async move {
                sqlx::query("INSERT INTO comments VALUES (random(), random())")
                    .execute(&mut tx)
                    .await
                    .unwrap();
            }),
        )
        .layer(axum_sqlx_tx::Layer::new_with_error::<MyError>(pool.clone()));

    let response = app
        .oneshot(
            http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = hyper::body::to_bytes(response.into_body()).await.unwrap();

    assert!(status.is_client_error());
    assert_eq!(body, "internal server error");
}

async fn insert_user(tx: &mut Tx, id: i32, name: &str) -> (i32, String) {
    let mut args = SqliteArguments::default();
    args.add(id);
    args.add(name);
    sqlx::query_as_with(
        r#"INSERT INTO users VALUES (?, ?) RETURNING id, name;"#,
        args,
    )
    .fetch_one(tx)
    .await
    .unwrap()
}

async fn get_users(pool: &sqlx::SqlitePool) -> Vec<(i32, String)> {
    sqlx::query_as("SELECT * FROM users")
        .fetch_all(pool)
        .await
        .unwrap()
}

struct Response {
    status: http::StatusCode,
    body: axum::body::Bytes,
}

async fn build_app<H, T>(handler: H) -> (NamedTempFile, sqlx::SqlitePool, Response)
where
    H: axum::handler::Handler<T, axum::body::Body>,
    T: 'static,
{
    let db = NamedTempFile::new().unwrap();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db.path().display()))
        .await
        .unwrap();

    sqlx::query("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT);")
        .execute(&pool)
        .await
        .unwrap();

    let app = axum::Router::new()
        .route("/", axum::routing::get(handler))
        .layer(axum_sqlx_tx::Layer::new(pool.clone()));

    let response = app
        .oneshot(
            http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = hyper::body::to_bytes(response.into_body()).await.unwrap();

    (db, pool, Response { status, body })
}

struct MyError(axum_sqlx_tx::Error);

impl From<axum_sqlx_tx::Error> for MyError {
    fn from(error: axum_sqlx_tx::Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for MyError {
    fn into_response(self) -> axum::response::Response {
        (http::StatusCode::IM_A_TEAPOT, "internal server error").into_response()
    }
}
