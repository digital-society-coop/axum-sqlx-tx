#[cfg(feature = "postgres")]
mod db;
mod layer;
mod slot;
mod tx;

pub use crate::{
    layer::{Layer, Service},
    tx::{Error, Tx},
};

#[cfg(test)]
mod tests {
    use std::{
        env,
        marker::PhantomData,
        ops::{Deref, DerefMut},
    };

    use axum::{error_handling::HandleErrorLayer, extract::FromRequest};
    use sqlx::{PgPool, Postgres};
    use tower::ServiceBuilder;

    use crate::{layer::Layer, Error, Tx};

    struct Auth<E>(PhantomData<E>);

    impl<B, E, H, C> FromRequest<B> for Auth<E>
    where
        B: Send + 'static,
        E: FromRequest<B> + Deref<Target = H> + DerefMut + Send,
        E::Rejection: std::fmt::Debug + Send,
        H: Deref<Target = C> + DerefMut + Send,
        C: sqlx::Connection,
    {
        type Rejection = Error;

        fn from_request<'life0, 'async_trait>(
            req: &'life0 mut axum::extract::RequestParts<B>,
        ) -> core::pin::Pin<
            Box<
                dyn core::future::Future<Output = Result<Self, Self::Rejection>>
                    + core::marker::Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let mut c = E::from_request(req).await.unwrap();
                c.ping().await.unwrap();

                Ok(Auth(PhantomData))
            })
        }
    }

    #[tokio::test]
    #[ignore]
    async fn transaction() {
        async fn handler(_auth: Auth<Tx<Postgres>>, mut tx: Tx<Postgres>) -> String {
            let (message,): (String,) = sqlx::query_as("SELECT 'hello world'")
                .fetch_one(&mut tx)
                .await
                .unwrap();
            message
        }

        let pool = PgPool::connect(&env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();

        let app = axum::Router::new()
            .route("/", axum::routing::get(handler))
            .route_layer(
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(|error: Error| async move { error }))
                    .layer(Layer::new(pool)),
            );

        let server = axum::Server::bind(&([0, 0, 0, 0], 0).into()).serve(app.into_make_service());
        println!("serving {}", server.local_addr());

        server.await.unwrap();
    }
}
