//! Access tokens. One `TokenProvider` per profile, each pinned to one identity.

mod az;

use anyhow::Result;
use futures::future::BoxFuture;

pub use az::AzCli;

pub const FABRIC: &str = "https://api.fabric.microsoft.com";
/// OneLake (DFS and table APIs) takes storage-audience tokens.
pub const STORAGE: &str = "https://storage.azure.com";
/// Microsoft Graph, for turning an email or group name into an object id.
pub const GRAPH: &str = "https://graph.microsoft.com";

pub trait TokenProvider: Send + Sync {
    /// A bearer token for `resource` that is valid for at least a few more minutes.
    fn token(&self, resource: &'static str) -> BoxFuture<'_, Result<String>>;

    /// The identity this provider signs in as, for display and error messages.
    fn account(&self) -> &str;
}
