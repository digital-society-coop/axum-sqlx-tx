//! Implementations of `sqlx` traits for extractors with known `sqlx::Database` types.
//!
//! Implementing the `sqlx` traits on the [`Tx`](crate::Tx) extractor makes it more ergonomic to use
//! in common situations, such as supplying an executor for `QueryAs`.

use futures::{future::BoxFuture, stream::BoxStream};

use crate::Tx;

macro_rules! impl_executor {
    ($db:path) => {
        impl<'c> sqlx::Executor<'c> for &'c mut Tx<$db> {
            type Database = $db;

            #[allow(clippy::type_complexity)]
            fn fetch_many<'e, 'q: 'e, E: 'q>(
                self,
                query: E,
            ) -> BoxStream<
                'e,
                Result<
                    sqlx::Either<
                        <Self::Database as sqlx::Database>::QueryResult,
                        <Self::Database as sqlx::Database>::Row,
                    >,
                    sqlx::Error,
                >,
            >
            where
                'c: 'e,
                E: sqlx::Execute<'q, Self::Database>,
            {
                (&mut **self).fetch_many(query)
            }

            fn fetch_optional<'e, 'q: 'e, E: 'q>(
                self,
                query: E,
            ) -> BoxFuture<'e, Result<Option<<Self::Database as sqlx::Database>::Row>, sqlx::Error>>
            where
                'c: 'e,
                E: sqlx::Execute<'q, Self::Database>,
            {
                (&mut **self).fetch_optional(query)
            }

            fn prepare_with<'e, 'q: 'e>(
                self,
                sql: &'q str,
                parameters: &'e [<Self::Database as sqlx::Database>::TypeInfo],
            ) -> BoxFuture<
                'e,
                Result<
                    <Self::Database as sqlx::database::HasStatement<'q>>::Statement,
                    sqlx::Error,
                >,
            >
            where
                'c: 'e,
            {
                (&mut **self).prepare_with(sql, parameters)
            }

            fn describe<'e, 'q: 'e>(
                self,
                sql: &'q str,
            ) -> BoxFuture<'e, Result<sqlx::Describe<Self::Database>, sqlx::Error>>
            where
                'c: 'e,
            {
                (&mut **self).describe(sql)
            }
        }
    };
}

#[cfg(feature = "any")]
impl_executor!(sqlx::Any);

#[cfg(feature = "mssql")]
impl_executor!(sqlx::Mssql);

#[cfg(feature = "mysql")]
impl_executor!(sqlx::MySql);

#[cfg(feature = "postgres")]
impl_executor!(sqlx::Postgres);

#[cfg(feature = "sqlite")]
impl_executor!(sqlx::Sqlite);
