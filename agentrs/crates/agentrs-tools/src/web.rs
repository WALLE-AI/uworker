//! Network-facing tool support: HTTP fetching, host policy, content
//! extraction, and web search backends.

pub mod fetch_tool;
mod html_to_md;
mod http_client;
mod search_backend;
mod search_brave;
mod search_duckduckgo;
mod search_searxng;
mod search_tavily;
pub mod search_tool;
mod url_cache;
mod url_policy;

pub use search_backend::{BackendError, SearchBackend, SearchHit, build_backend};
