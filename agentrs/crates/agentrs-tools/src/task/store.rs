use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::task::model::{Task, TaskDraft, TaskError, TaskPatch, TaskStatus, join_ids};

const STATE_FILE: &str = "tasks.json";

/// The whole task list as it sits on disk.
#[derive(Debug, Serialize, Deserialize)]
struct TaskFile {
    /// Next id to hand out. Persisted rather than derived from the current
    /// tasks so a deleted task's id is never reused, which would silently
    /// re-point every dependency that still names it.
    #[serde(default = "first_id")]
    next_id: u64,
    #[serde(default)]
    tasks: Vec<Task>,
}

fn first_id() -> u64 {
    1
}

// Hand-written rather than derived: a derived `Default` would start `next_id`
// at 0, and the serde attribute above only covers deserialization. An empty
// in-memory graph has to agree with a freshly parsed one, or the very first
// task gets id "0" while a reopened store starts at "1".
impl Default for TaskFile {
    fn default() -> Self {
        Self {
            next_id: first_id(),
            tasks: Vec::new(),
        }
    }
}

/// File-backed task graph.
///
/// Every operation is a read-modify-write under an exclusive lock on the state
/// file itself, which is what makes "claim this task" atomic. agentrs spawns
/// sub-agents in-process rather than as subprocesses, so an in-process mutex
/// would cover today's concurrency; the OS lock is used anyway so a second
/// agentrs sharing the workspace cannot interleave a half-written graph.
#[derive(Debug)]
pub struct TaskStore {
    dir: PathBuf,
}

impl TaskStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    /// Run `mutate` against the stored graph, persisting it if it succeeds.
    ///
    /// The lock is held for the whole read-modify-write, so a concurrent caller
    /// either sees the state before this call or the state after it, never a
    /// partially applied change.
    fn with_state<T>(&self, mutate: impl FnOnce(&mut TaskFile) -> Result<T, TaskError>) -> Result<T, TaskError> {
        fs::create_dir_all(&self.dir).map_err(storage)?;

        let path = self.state_path();
        let mut handle = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(storage)?;
        handle.lock().map_err(storage)?;

        let mut state = read_state(&mut handle)?;
        let outcome = mutate(&mut state)?;
        write_state(&mut handle, &state)?;
        Ok(outcome)
    }

    /// Read the graph without taking the write path.
    fn read_only<T>(&self, read: impl FnOnce(&TaskFile) -> T) -> Result<T, TaskError> {
        let path = self.state_path();
        let Ok(mut handle) = File::open(&path) else {
            // No file yet simply means no tasks.
            return Ok(read(&TaskFile::default()));
        };
        handle.lock_shared().map_err(storage)?;
        let state = read_state(&mut handle)?;
        Ok(read(&state))
    }

    /// Append new tasks, assigning ids in the order given.
    pub fn create(&self, drafts: Vec<TaskDraft>) -> Result<Vec<Task>, TaskError> {
        self.with_state(|state| {
            let mut created = Vec::with_capacity(drafts.len());
            for draft in drafts {
                let subject = draft.subject.trim().to_string();
                if subject.is_empty() {
                    return Err(TaskError::EmptySubject);
                }

                let id = state.next_id.to_string();
                state.next_id += 1;

                let task = Task {
                    id: id.clone(),
                    subject,
                    description: draft.description.trim().to_string(),
                    active_form: normalize_optional(draft.active_form),
                    owner: normalize_optional(draft.owner),
                    status: TaskStatus::Pending,
                    blocks: Vec::new(),
                    blocked_by: Vec::new(),
                };
                state.tasks.push(task);

                // Dependencies are applied after insertion so a batch can name
                // a sibling created in the same call.
                for blocker in draft.blocked_by {
                    add_dependency(state, &id, &blocker)?;
                }
                created.push(id);
            }

            let created: Vec<Task> = created
                .into_iter()
                .filter_map(|id| state.tasks.iter().find(|task| task.id == id).cloned())
                .collect();
            debug!(target: "agentrs_tools", count = created.len(), "tasks created");
            Ok(created)
        })
    }

    pub fn list(&self) -> Result<Vec<Task>, TaskError> {
        self.read_only(|state| state.tasks.clone())
    }

    pub fn get(&self, id: &str) -> Result<Option<Task>, TaskError> {
        self.read_only(|state| state.tasks.iter().find(|task| task.id == id).cloned())
    }

    /// Apply a patch to one task.
    pub fn update(&self, id: &str, patch: TaskPatch) -> Result<Task, TaskError> {
        self.with_state(|state| {
            if !state.tasks.iter().any(|task| task.id == id) {
                return Err(TaskError::NotFound { id: id.to_string() });
            }

            for blocker in &patch.remove_blocked_by {
                remove_dependency(state, id, blocker);
            }
            for blocker in &patch.add_blocked_by {
                add_dependency(state, id, blocker)?;
            }

            // Checked against the post-edit graph, so a call that removes a
            // dependency and starts the task in one go is judged on the
            // dependencies it actually leaves behind.
            if let Some(status) = patch.status
                && status != TaskStatus::Pending
            {
                let open = open_blockers(state, id);
                if !open.is_empty() {
                    return Err(TaskError::Blocked {
                        id: id.to_string(),
                        status: status.as_str(),
                        blockers: join_ids(&open),
                    });
                }
            }

            let task = state
                .tasks
                .iter_mut()
                .find(|task| task.id == id)
                .ok_or_else(|| TaskError::NotFound { id: id.to_string() })?;

            if let Some(subject) = patch.subject {
                let subject = subject.trim().to_string();
                if subject.is_empty() {
                    return Err(TaskError::EmptySubject);
                }
                task.subject = subject;
            }
            if let Some(description) = patch.description {
                task.description = description.trim().to_string();
            }
            if let Some(active_form) = patch.active_form {
                task.active_form = normalize_optional(Some(active_form));
            }
            if let Some(owner) = patch.owner {
                task.owner = normalize_optional(Some(owner));
            }
            if let Some(status) = patch.status {
                task.status = status;
            }

            Ok(task.clone())
        })
    }

    /// Remove a task and every edge naming it.
    pub fn delete(&self, id: &str) -> Result<(), TaskError> {
        self.with_state(|state| {
            if !state.tasks.iter().any(|task| task.id == id) {
                return Err(TaskError::NotFound { id: id.to_string() });
            }
            state.tasks.retain(|task| task.id != id);
            for task in &mut state.tasks {
                task.blocks.retain(|other| other != id);
                task.blocked_by.retain(|other| other != id);
            }
            debug!(target: "agentrs_tools", %id, "task deleted");
            Ok(())
        })
    }
}

/// Ids of the tasks blocking `id` that are not finished yet.
fn open_blockers(state: &TaskFile, id: &str) -> Vec<String> {
    let by_id: HashMap<&str, &Task> = state.tasks.iter().map(|task| (task.id.as_str(), task)).collect();
    state
        .tasks
        .iter()
        .find(|task| task.id == id)
        .map(|task| {
            task.blocked_by
                .iter()
                .filter(|blocker| by_id.get(blocker.as_str()).is_some_and(|task| task.is_open()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Record that `blocker` must finish before `id`, updating both sides.
fn add_dependency(state: &mut TaskFile, id: &str, blocker: &str) -> Result<(), TaskError> {
    if id == blocker {
        return Err(TaskError::SelfDependency { id: id.to_string() });
    }
    if !state.tasks.iter().any(|task| task.id == blocker) {
        return Err(TaskError::NotFound {
            id: blocker.to_string(),
        });
    }
    if let Some(cycle) = dependency_cycle(state, id, blocker) {
        return Err(TaskError::DependencyCycle { cycle });
    }

    if let Some(task) = state.tasks.iter_mut().find(|task| task.id == id)
        && !task.blocked_by.iter().any(|existing| existing == blocker)
    {
        task.blocked_by.push(blocker.to_string());
    }
    if let Some(task) = state.tasks.iter_mut().find(|task| task.id == blocker)
        && !task.blocks.iter().any(|existing| existing == id)
    {
        task.blocks.push(id.to_string());
    }
    Ok(())
}

fn remove_dependency(state: &mut TaskFile, id: &str, blocker: &str) {
    if let Some(task) = state.tasks.iter_mut().find(|task| task.id == id) {
        task.blocked_by.retain(|existing| existing != blocker);
    }
    if let Some(task) = state.tasks.iter_mut().find(|task| task.id == blocker) {
        task.blocks.retain(|existing| existing != id);
    }
}

/// The path proving that making `id` wait on `blocker` closes a loop.
///
/// `blocker` already reaching `id` through existing edges is exactly what makes
/// the new edge circular, so the search runs forward from `blocker`.
fn dependency_cycle(state: &TaskFile, id: &str, blocker: &str) -> Option<String> {
    let blocked_by: HashMap<&str, &[String]> = state
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task.blocked_by.as_slice()))
        .collect();

    let mut previous: HashMap<&str, &str> = HashMap::new();
    let mut seen: HashSet<&str> = HashSet::from([blocker]);
    let mut queue = VecDeque::from([blocker]);

    while let Some(current) = queue.pop_front() {
        for next in blocked_by.get(current).copied().unwrap_or(&[]) {
            let next = next.as_str();
            if next == id {
                // Walk back to render the loop in the order a reader follows it.
                let mut path = vec![id, current];
                let mut cursor = current;
                while let Some(step) = previous.get(cursor) {
                    path.push(step);
                    cursor = step;
                }
                path.push(id);
                path.reverse();
                return Some(path.join(" -> "));
            }
            if seen.insert(next) {
                previous.insert(next, current);
                queue.push_back(next);
            }
        }
    }
    None
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn read_state(handle: &mut File) -> Result<TaskFile, TaskError> {
    handle.seek(SeekFrom::Start(0)).map_err(storage)?;
    let mut text = String::new();
    handle.read_to_string(&mut text).map_err(storage)?;
    if text.trim().is_empty() {
        return Ok(TaskFile::default());
    }
    serde_json::from_str(&text).map_err(|error| TaskError::Storage {
        reason: format!("task file is unreadable: {error}"),
    })
}

fn write_state(handle: &mut File, state: &TaskFile) -> Result<(), TaskError> {
    let text = serde_json::to_string_pretty(state).map_err(|error| TaskError::Storage {
        reason: error.to_string(),
    })?;
    handle.seek(SeekFrom::Start(0)).map_err(storage)?;
    handle.set_len(0).map_err(storage)?;
    handle.write_all(text.as_bytes()).map_err(storage)?;
    handle.flush().map_err(storage)?;
    Ok(())
}

fn storage(error: std::io::Error) -> TaskError {
    TaskError::Storage {
        reason: error.to_string(),
    }
}

/// Where a workspace keeps its task graph.
pub fn task_dir(workspace: &Path) -> PathBuf {
    workspace.join(".agentrs").join("tasks")
}

#[cfg(test)]
#[path = "store_test.rs"]
mod store_test;
