use std::sync::{Arc, RwLock};

use crate::file_cache::FileStateCache;

/// Mutable state owned by one agent engine and referenced by reusable tools.
#[derive(Clone, Default)]
pub struct ToolContext {
    file_cache: Option<Arc<RwLock<FileStateCache>>>,
}

impl ToolContext {
    pub fn new(file_cache: Option<Arc<RwLock<FileStateCache>>>) -> Self {
        Self { file_cache }
    }

    pub fn file_cache(&self) -> Option<&Arc<RwLock<FileStateCache>>> {
        self.file_cache.as_ref()
    }
}
