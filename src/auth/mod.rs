//! Access tokens. One `TokenProvider` per profile, each pinned to one identity.

mod az;

use anyhow::Result;
use futures::future::BoxFuture;

pub use az::AzCli;

pub const FABRIC: &str = "https://api.fabric.microsoft.com";

pub trait TokenProvider: Send + Sync {
    /// A bearer token for `resource` that is valid for at least a few more minutes.
    fn token(&self, resource: &'static str) -> BoxFuture<'_, Result<String>>;

    /// The identity this provider signs in as, for display and error messages.
    fn account(&self) -> &str;
}
