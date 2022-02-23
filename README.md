# `axum-sqlx-rx`

Request-bound [SQLx](https://github.com/launchbadge/sqlx) transactions for [axum](https://github.com/tokio-rs/axum).

## Summary

`axum-sqlx-rx` provides an `axum` [extractor](https://docs.rs/axum/latest/axum/#extractors) for obtaining a request-bound transaction.
The transaction begins the first time the extractor is used, and is stored with the request for use by other middleware/handlers.
The transaction is resolved depending on the status code of the response – successful (`2XX`) responses will commit the transaction, otherwise it will be rolled back.

See the [crate documentation](https://docs.rs/axum-sqlx-tx) for more information and examples.
