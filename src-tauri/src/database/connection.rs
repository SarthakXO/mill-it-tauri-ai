use super::DatabaseError;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{env, time::Duration};

pub(crate) struct Database {
    pool: Option<PgPool>,
}

impl Database {
    pub(crate) async fn from_environment() -> Self {
        let _ = dotenvy::dotenv();
        let pool = env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())
            .and_then(|url| {
                PgPoolOptions::new()
                    .max_connections(4)
                    .min_connections(0)
                    .acquire_timeout(Duration::from_secs(5))
                    .idle_timeout(Duration::from_secs(60))
                    .connect_lazy(&url)
                    .ok()
            });

        Self { pool }
    }

    pub(super) fn pool(&self) -> Result<&PgPool, DatabaseError> {
        self.pool.as_ref().ok_or(DatabaseError::Configuration)
    }
}
