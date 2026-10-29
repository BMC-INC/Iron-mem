//! Runtime SQLx surface used by IronMem, limited to its two supported drivers.
//!
//! The upstream umbrella crate resolves unused MySQL/RSA packages into the lock
//! file. Keep these direct dependencies pinned together because SQLx internals
//! do not promise independent semantic-version compatibility.

pub use sqlx_core::any::{Any, AnyConnection, AnyPool};
pub use sqlx_core::error::Error;
pub use sqlx_core::query::query;
pub use sqlx_core::query_scalar::query_scalar;
pub use sqlx_core::row::Row;
pub use sqlx_core::transaction::Transaction;

pub mod any {
    pub use sqlx_core::any::{AnyPoolOptions, AnyRow};

    /// Install only the SQLite and PostgreSQL runtime drivers, once per process.
    pub fn install_default_drivers() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            sqlx_core::any::driver::install_drivers(&[
                sqlx_sqlite::any::DRIVER,
                sqlx_postgres::any::DRIVER,
            ])
            .expect("IronMem SQLx drivers are installed once");
        });
    }
}
