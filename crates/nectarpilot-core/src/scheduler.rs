use std::{collections::HashSet, sync::Arc};

use parking_lot::Mutex;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskKey {
    pub profile_id: Uuid,
    pub task_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct TaskScheduler {
    active: Arc<Mutex<HashSet<TaskKey>>>,
}

impl TaskScheduler {
    pub fn acquire(&self, key: TaskKey) -> Result<TaskPermit, ScheduleError> {
        let mut active = self.active.lock();
        if !active.insert(key.clone()) {
            return Err(ScheduleError::Duplicate(key));
        }
        Ok(TaskPermit {
            key: Some(key),
            active: Arc::clone(&self.active),
        })
    }

    #[must_use]
    pub fn is_active(&self, key: &TaskKey) -> bool {
        self.active.lock().contains(key)
    }
}

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error("task {0:?} is already scheduled")]
    Duplicate(TaskKey),
}

#[derive(Debug)]
pub struct TaskPermit {
    key: Option<TaskKey>,
    active: Arc<Mutex<HashSet<TaskKey>>>,
}

impl Drop for TaskPermit {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.active.lock().remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{TaskKey, TaskScheduler};

    #[test]
    fn duplicate_task_is_rejected_until_permit_drops() {
        let scheduler = TaskScheduler::default();
        let key = TaskKey {
            profile_id: Uuid::nil(),
            task_name: "gather".into(),
        };
        let permit = scheduler.acquire(key.clone()).expect("first task");
        assert!(scheduler.acquire(key.clone()).is_err());
        drop(permit);
        assert!(scheduler.acquire(key).is_ok());
    }
}
