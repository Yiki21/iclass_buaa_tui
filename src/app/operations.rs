use super::*;

/// Captures the originating account before a task starts. Writes are not
/// cancelled by logout: cancelling locally cannot roll back a submitted write.
#[derive(Clone)]

pub(super) struct TaskSender {
    tx:         UnboundedSender<AsyncEvent>,
    generation: u64,
    read_jobs:  std::sync::Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    write:      bool,
}

impl TaskSender {
    pub(super) fn write(mut self) -> Self {

        self.write = true;

        self
    }

    pub(super) fn send(
        &self,
        event: AsyncEvent,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<AsyncEvent>> {

        self.tx.send(AsyncEvent::Scoped {
            generation: self.generation,
            event:      Box::new(event),
        })
    }

    pub(super) fn spawn<F, Fut>(self, task: F)
    where
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {

        let read_jobs = self.read_jobs.clone();

        let write = self.write;

        let handle = tokio::spawn(task(self));

        if !write {

            let mut jobs = read_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            jobs.retain(|job| !job.is_finished());

            jobs.push(handle.abort_handle());
        }
    }
}

impl App {
    pub(super) fn task_sender(&self, tx: &UnboundedSender<AsyncEvent>) -> TaskSender {

        TaskSender {
            tx:         tx.clone(),
            generation: self.session_generation,
            read_jobs:  self.read_jobs.clone(),
            write:      false,
        }
    }
}
