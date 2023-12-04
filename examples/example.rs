//! A silly server that generates random numers, but only commits positive ones.

use std::error::Error;
use std::net::SocketAddr;

use axum::{response::IntoResponse, routing::get, Json};
use http::StatusCode;

// OPTIONAL: use a type alias to avoid repeating your database type
type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // You can use any sqlx::Pool
    let db = tempfile::NamedTempFile::new()?;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db.path().display())).await?;

    // Create a table (in a real application you might run migrations)
    sqlx::query("CREATE TABLE IF NOT EXISTS numbers (number INT PRIMARY KEY);")
        .execute(&pool)
        .await?;

    // Standard axum app setup
    let app = axum::Router::new()
        .route("/numbers", get(list_numbers).post(generate_number))
        // Apply the Tx middleware
        .layer(axum_sqlx_tx::Layer::new(pool.clone()));

    let addr: SocketAddr = ([0, 0, 0, 0], 0).into();
    let server = axum::serve(
        tokio::net::TcpListener::bind(&addr).await.unwrap(),
        app.into_make_service(),
    );

    println!("Listening on {}", addr);
    server.await?;

    Ok(())
}

async fn list_numbers(mut tx: Tx) -> Result<Json<Vec<i32>>, DbError> {
    let numbers: Vec<(i32,)> = sqlx::query_as("SELECT * FROM numbers")
        .fetch_all(&mut tx)
        .await?;

    Ok(Json(numbers.into_iter().map(|n| n.0).collect()))
}

async fn generate_number(mut tx: Tx) -> Result<(StatusCode, Json<i32>), DbError> {
    let (number,): (i32,) =
        sqlx::query_as("INSERT INTO numbers VALUES (random()) RETURNING number;")
            .fetch_one(&mut tx)
            .await?;

    // Simulate a possible error – in reality this could be something like interacting with another
    // service, or running another query.
    let status = if number > 0 {
        StatusCode::OK
    } else {
        StatusCode::IM_A_TEAPOT
    };

    // no need to explicitly resolve!
    Ok((status, Json(number)))
}

// An sqlx::Error wrapper that implements IntoResponse
struct DbError(sqlx::Error);

impl From<sqlx::Error> for DbError {
    fn from(error: sqlx::Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for DbError {
    fn into_response(self) -> axum::response::Response {
        println!("ERROR: {}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
    }
}
