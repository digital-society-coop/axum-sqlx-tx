use axum::{middleware, response::IntoResponse};
use axum_sqlx_tx::State;
use http_body_util::BodyExt;
use sqlx::{sqlite::SqliteArguments, Arguments as _};
use tower::ServiceExt;

type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;

#[tokio::test]
async fn commit_on_success() {
    let (pool, response) = build_app(|mut tx: Tx| async move {
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
    let (pool, response) = build_app(|mut tx: Tx| async move {
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
    let (pool, response) = build_app(|mut tx: Tx| async move {
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
    let (pool, response) = build_app(|mut tx: Tx| async move {
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
async fn extract_from_middleware_and_handler() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    sqlx::query("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT);")
        .execute(&pool)
        .await
        .unwrap();

    async fn test_middleware(
        mut tx: Tx,
        req: http::Request<axum::body::Body>,
        next: middleware::Next,
    ) -> impl IntoResponse {
        insert_user(&mut tx, 1, "bobby tables").await;

        // If we explicitly drop `tx` it should be consumable from the next handler.
        drop(tx);
        next.run(req).await
    }

    let (state, layer) = Tx::setup(pool);

    let app = axum::Router::new()
        .route(
            "/",
            axum::routing::get(|mut tx: Tx| async move {
                let users: Vec<(i32, String)> = sqlx::query_as("SELECT * FROM users")
                    .fetch_all(&mut tx)
                    .await
                    .unwrap();
                axum::Json(users)
            }),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            test_middleware,
        ))
        .layer(layer)
        .with_state(state);

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
    let body = response.into_body().collect().await.unwrap().to_bytes();

    assert!(status.is_success());
    assert_eq!(body.as_ref(), b"[[1,\"bobby tables\"]]");
}

#[tokio::test]
async fn middleware_cloning_request_extensions() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    async fn test_middleware(
        req: http::Request<axum::body::Body>,
        next: middleware::Next,
    ) -> impl IntoResponse {
        // Hold a clone of the request extensions
        let _extensions = req.extensions().clone();

        next.run(req).await
    }

    let (state, layer) = Tx::setup(pool);

    let app = axum::Router::new()
        .route("/", axum::routing::get(|_tx: Tx| async move {}))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            test_middleware,
        ))
        .layer(layer)
        .with_state(state);

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
    let body = response.into_body().collect().await.unwrap().to_bytes();
    dbg!(body);

    assert!(status.is_success());
}

#[tokio::test]
async fn substates() {
    #[derive(Clone)]
    struct MyState {
        state: State<sqlx::Sqlite>,
    }

    impl axum_core::extract::FromRef<MyState> for State<sqlx::Sqlite> {
        fn from_ref(state: &MyState) -> Self {
            state.state.clone()
        }
    }

    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    let (state, layer) = Tx::setup(pool);

    let app = axum::Router::new()
        .route("/", axum::routing::get(|_: Tx| async move {}))
        .layer(layer)
        .with_state(MyState { state });
    let response = app
        .oneshot(
            http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_success());
}

#[tokio::test]
async fn missing_layer() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    // Note that we have to explicitly ignore the `_layer`, making it hard to do this accidentally.
    let (state, _layer) = Tx::setup(pool);

    let app = axum::Router::new()
        .route("/", axum::routing::get(|_: Tx| async move {}))
        .with_state(state);
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

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body, format!("{}", axum_sqlx_tx::Error::MissingExtension));
}

#[tokio::test]
async fn overlapping_extractors() {
    let (_, response) = build_app(|_: Tx, _: Tx| async move {}).await;

    assert!(response.status.is_server_error());
    assert_eq!(
        response.body,
        format!("{}", axum_sqlx_tx::Error::OverlappingExtractors)
    );
}

#[tokio::test]
async fn extractor_error_override() {
    let (_, response) =
        build_app(|_: Tx, _: axum_sqlx_tx::Tx<sqlx::Sqlite, MyExtractorError>| async move {}).await;

    assert!(response.status.is_client_error());
    assert_eq!(response.body, "internal server error");
}

#[tokio::test]
async fn layer_error_override() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

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

    let (state, layer) = Tx::config(pool).layer_error::<MyLayerError>().setup();

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
        .layer(layer)
        .with_state(state);

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
    let body = response.into_body().collect().await.unwrap().to_bytes();

    assert!(status.is_client_error());
    assert_eq!(body, "internal server error");
}

#[tokio::test]
async fn multi_db() {
    #[derive(Debug)]
    struct DbA;
    impl axum_sqlx_tx::Marker for DbA {
        type Driver = sqlx::Sqlite;
    }
    type TxA = axum_sqlx_tx::Tx<DbA>;

    #[derive(Debug)]
    struct DbB;
    impl axum_sqlx_tx::Marker for DbB {
        type Driver = sqlx::Sqlite;
    }
    type TxB = axum_sqlx_tx::Tx<DbB>;

    let pool_a = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let pool_b = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    sqlx::query("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY);")
        .execute(&pool_a)
        .await
        .unwrap();
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS comments (
            id INT PRIMARY KEY,
            user_id INT
        );"#,
    )
    .execute(&pool_b)
    .await
    .unwrap();

    let (state_a, layer_a) = TxA::setup(pool_a);
    let (state_b, layer_b) = TxB::setup(pool_b);

    #[derive(Clone)]
    struct State {
        state_a: axum_sqlx_tx::State<DbA>,
        state_b: axum_sqlx_tx::State<DbB>,
    }

    impl axum::extract::FromRef<State> for axum_sqlx_tx::State<DbA> {
        fn from_ref(input: &State) -> Self {
            input.state_a.clone()
        }
    }

    impl axum::extract::FromRef<State> for axum_sqlx_tx::State<DbB> {
        fn from_ref(input: &State) -> Self {
            input.state_b.clone()
        }
    }

    let app = axum::Router::new()
        .route(
            "/",
            axum::routing::get(|mut tx_a: TxA, mut tx_b: TxB| async move {
                sqlx::query("SELECT * FROM users")
                    .execute(&mut tx_a)
                    .await
                    .unwrap();
                sqlx::query("SELECT * FROM comments")
                    .execute(&mut tx_b)
                    .await
                    .unwrap();
            }),
        )
        .layer(layer_a)
        .layer(layer_b)
        .with_state(State { state_a, state_b });

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

    assert!(status.is_success());
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

async fn build_app<H, T>(handler: H) -> (sqlx::SqlitePool, Response)
where
    H: axum::handler::Handler<T, State<sqlx::Sqlite>>,
    T: 'static,
{
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    sqlx::query("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT);")
        .execute(&pool)
        .await
        .unwrap();

    let (state, layer) = Tx::setup(pool.clone());

    let app = axum::Router::new()
        .route("/", axum::routing::get(handler))
        .layer(layer)
        .with_state(state);

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
    let body = response.into_body().collect().await.unwrap().to_bytes();

    (pool, Response { status, body })
}

struct MyExtractorError(axum_sqlx_tx::Error);

impl From<axum_sqlx_tx::Error> for MyExtractorError {
    fn from(error: axum_sqlx_tx::Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for MyExtractorError {
    fn into_response(self) -> axum::response::Response {
        (http::StatusCode::IM_A_TEAPOT, "internal server error").into_response()
    }
}

struct MyLayerError(sqlx::Error);

impl From<sqlx::Error> for MyLayerError {
    fn from(error: sqlx::Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for MyLayerError {
    fn into_response(self) -> axum::response::Response {
        (http::StatusCode::IM_A_TEAPOT, "internal server error").into_response()
    }
}
