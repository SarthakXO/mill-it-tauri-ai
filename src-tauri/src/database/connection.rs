use super::DatabaseError;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{env, time::Duration};

/*
 * DATABASE_URL available when the Rust binary is compiled.
 *
 * For the packaged internal V1 this becomes the fallback database URL
 * when the macOS app is launched from Finder and has no shell environment.
 */
const BUNDLED_DATABASE_URL: Option<&str> = option_env!("DATABASE_URL");

pub(crate) struct Database {
    pool: Option<PgPool>,
}

impl Database {
    pub(crate) async fn from_environment() -> Self {
        /*
         * Development behavior remains unchanged.
         *
         * When running `yarn tauri dev`, this can load DATABASE_URL
         * from the project's .env file.
         */
        let _ = dotenvy::dotenv();

        /*
         * Runtime configuration has priority.
         *
         * This means you can still override DATABASE_URL on a machine
         * without rebuilding the application.
         */
        let runtime_url = env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty());

        /*
         * Packaged-app fallback.
         *
         * This URL was embedded when `cargo` compiled the application.
         */
        let bundled_url = BUNDLED_DATABASE_URL
            .map(str::to_owned)
            .filter(|url| !url.trim().is_empty());

        let database_url = runtime_url.or(bundled_url);

        let pool = database_url.and_then(|url| {
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
        self.pool
            .as_ref()
            .ok_or(DatabaseError::Configuration)
    }
}