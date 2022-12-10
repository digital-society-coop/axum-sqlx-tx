use std::str::FromStr;

use axum::{error_handling::HandleErrorLayer, handler::Handler};
use axum_sqlx_tx::TxLayerError;
use http::StatusCode;
use sqlx::{
    sqlite::{SqliteArguments, SqliteConnectOptions},
    Arguments as _, SqlitePool,
};
use tower::{ServiceBuilder, ServiceExt};

type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;

#[tokio::test]
async fn commit_on_success() {
    // arrange
    let pool =
        init_database(&["CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT);"])
            .await
            .unwrap();

    // act
    let response = call_handler(pool.clone(), |mut tx: Tx| async move {
        let (_, name) = insert_user(&mut tx, 1, "huge hackerman").await;
        format!("hello {name}")
    })
    .await;

    // assert
    assert!(response.status.is_success());
    assert_eq!(response.body, "hello huge hackerman");

    let users: Vec<(i32, String)> = sqlx::query_as("SELECT * FROM users")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(users, vec![(1, "huge hackerman".to_string())]);
}

#[tokio::test]
async fn rollback_on_error() {
    // arrange
    let pool =
        init_database(&["CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT);"])
            .await
            .unwrap();

    // act
    let response = call_handler(pool.clone(), |mut tx: Tx| async move {
        insert_user(&mut tx, 1, "michael oxmaul").await;
        http::StatusCode::BAD_REQUEST
    })
    .await;

    // assert
    assert!(response.status.is_client_error());
    assert!(response.body.is_empty());

    assert_eq!(get_users(&pool).await, vec![]);
}

#[tokio::test]
async fn explicit_commit() {
    // arrange
    let pool =
        init_database(&["CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT);"])
            .await
            .unwrap();

    // act
    let response = call_handler(pool.clone(), |mut tx: Tx| async move {
        insert_user(&mut tx, 1, "michael oxmaul").await;
        tx.commit().await.unwrap();
        http::StatusCode::BAD_REQUEST
    })
    .await;

    // assert
    assert!(response.status.is_client_error());
    assert!(response.body.is_empty());

    assert_eq!(
        get_users(&pool).await,
        vec![(1, "michael oxmaul".to_string())]
    );
}

#[tokio::test]
async fn missing_layer() {
    //arrange
    let app = axum::Router::new().route("/", axum::routing::get(|_: Tx| async move {}));

    // act
    let response = app
        .oneshot(
            http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // assert
    assert!(response.status().is_server_error());

    let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
    assert_eq!(
        body,
        format!("{}", axum_sqlx_tx::TxRejection::MissingExtension)
    );
}

#[tokio::test]
async fn overlapping_extractors() {
    // arrange
    let pool = init_database(&[]).await.unwrap();

    // act
    let response = call_handler(pool.clone(), |_: Tx, _: Tx| async move {}).await;

    // assert
    assert!(response.status.is_server_error());
    assert_eq!(
        response.body,
        format!("{}", axum_sqlx_tx::TxRejection::OverlappingExtractors)
    );
}

#[tokio::test]
async fn rollback_during_commit_error_on_tx_layer() {
    // arrange
    let pool = init_database(&[
        "CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY);",
        r#"
            CREATE TABLE IF NOT EXISTS comments (
                id INT PRIMARY KEY,
                user_id INT,
                FOREIGN KEY (user_id) REFERENCES users(id) DEFERRABLE INITIALLY DEFERRED
            );
        "#,
    ])
    .await
    .unwrap();

    // act
    let response = call_handler(pool.clone(), |mut tx: Tx| async move {
        sqlx::query("INSERT INTO users VALUES (1)")
            .execute(&mut tx)
            .await
            .unwrap();

        sqlx::query("INSERT INTO comments VALUES (1, random())")
            .execute(&mut tx)
            .await
            .unwrap();
    })
    .await;

    // assert
    assert!(response.status.is_server_error());
    assert_eq!(response.body, "error during commit");

    let users_count: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    let comments_count: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM comments")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(users_count, 0);
    assert_eq!(comments_count, 0);
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

async fn init_database(init_statements: &[&str]) -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true),
    )
    .await?;

    for &statement in init_statements {
        sqlx::query(statement).execute(&pool).await?;
    }

    Ok(pool)
}

async fn call_handler<T: 'static>(pool: SqlitePool, handler: impl Handler<T, ()>) -> Response {
    let app = axum::Router::new()
        .route("/", axum::routing::get(handler))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|_err: TxLayerError| async move {
                    (StatusCode::INTERNAL_SERVER_ERROR, "error during commit")
                }))
                .layer(axum_sqlx_tx::Layer::new(pool.clone())),
        )
        .with_state(());

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

    Response { status, body }
}
