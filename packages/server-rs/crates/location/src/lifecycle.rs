use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::watch;

use crate::events::LocationEvent;
use crate::location::LocationInfo;
use crate::project::{ProjectError, ProjectService};

pub struct Lifecycle {
    directory: String,
    workspace_id: Option<String>,
    project_id: String,
    project: Arc<ProjectService>,
    done: watch::Sender<bool>,
    closed: AtomicBool,
    retained: Arc<AtomicUsize>,
}

impl Lifecycle {
    pub async fn start(project: Arc<ProjectService>, info: &LocationInfo) -> Result<Lifecycle, ProjectError> {
        project.activate(&info.project.id).await?;
        let (done, _) = watch::channel(false);
        Ok(Lifecycle {
            directory: info.directory.clone(),
            workspace_id: info.workspace_id.clone(),
            project_id: info.project.id.clone(),
            project,
            done,
            closed: AtomicBool::new(false),
            retained: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub async fn shutdown(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.project.publish(LocationEvent::LocationShutdown {
            directory: self.directory.clone(),
            workspace_id: self.workspace_id.clone(),
        });
        self.done.send_replace(true);
    }

    pub async fn await_shutdown(&self) {
        let mut receiver = self.done.subscribe();
        if *receiver.borrow() {
            return;
        }
        let _ = receiver.wait_for(|done| *done).await;
    }

    pub fn retain(&self) -> RetainGuard {
        self.retained.fetch_add(1, Ordering::SeqCst);
        RetainGuard { retained: self.retained.clone() }
    }

    pub fn is_retained(&self) -> bool {
        self.retained.load(Ordering::SeqCst) > 0
    }

    pub async fn session_event(&self) -> Result<(), ProjectError> {
        self.project.activate(&self.project_id).await
    }
}

pub struct RetainGuard {
    retained: Arc<AtomicUsize>,
}

impl Drop for RetainGuard {
    fn drop(&mut self) {
        self.retained.fetch_sub(1, Ordering::SeqCst);
    }
}
