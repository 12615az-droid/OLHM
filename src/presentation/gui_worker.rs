//! A single-slot mailbox keeps slow rendering from accumulating samples.
//! Sessions are created and polled on the worker, never on the window thread.
use crate::{model::MonitorSnapshot, monitor::MonitorSession};
use std::{
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type Message = Result<MonitorSnapshot, String>;

pub(super) struct Worker {
    latest: Arc<Mutex<Option<Message>>>,
    stop: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl Worker {
    pub(super) fn start(
        interval: Duration,
        notify: impl Fn() + Send + 'static,
        open: impl FnOnce() -> Result<Box<dyn MonitorSession>, Box<dyn std::error::Error>>
        + Send
        + 'static,
    ) -> std::io::Result<Self> {
        let latest = Arc::new(Mutex::new(None));
        let mailbox = Arc::clone(&latest);
        let (stop, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("olhm-monitor".into())
            .spawn(move || {
                let publish = |message| {
                    *mailbox.lock().unwrap() = Some(message);
                    notify();
                };
                let mut session = match open() {
                    Ok(session) => session,
                    Err(error) => {
                        publish(Err(error.to_string()));
                        return;
                    }
                };
                loop {
                    match receiver.try_recv() {
                        Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                    let started = Instant::now();
                    publish(Ok(session.sample()));
                    match receiver.recv_timeout(interval.saturating_sub(started.elapsed())) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            })?;
        Ok(Self {
            latest,
            stop,
            thread,
        })
    }

    pub(super) fn take(&self) -> Option<Message> {
        // Rendering never waits for discovery or sampling, even during shutdown.
        self.latest.try_lock().ok()?.take()
    }

    pub(super) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Interrupt even a 60-second interval. Do not join a potentially blocked
        // kernel read on the window thread; the process owns no external service.
        let _ = self.stop.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Device, DeviceKind};

    struct Session {
        device: Device,
        sample_number: u64,
        gate: mpsc::Receiver<()>,
    }
    impl MonitorSession for Session {
        fn device(&self) -> &Device {
            &self.device
        }
        fn sample(&mut self) -> MonitorSnapshot {
            self.gate.recv_timeout(Duration::from_secs(2)).unwrap();
            self.sample_number += 1;
            MonitorSnapshot {
                interval: Some(Duration::from_secs(self.sample_number)),
                devices: Vec::new(),
            }
        }
    }

    #[test]
    fn slow_sampling_does_not_block_ui_and_shutdown_interrupts_long_interval() {
        let (gate, receiver) = mpsc::channel();
        let (notify, updated) = mpsc::channel();
        let (opened, ready) = mpsc::channel();
        let worker = Worker::start(
            Duration::from_secs(60),
            move || {
                let _ = notify.send(());
            },
            move || {
                opened.send(()).unwrap();
                Ok(Box::new(Session {
                    device: Device {
                        id: "test".into(),
                        parent_id: None,
                        kind: DeviceKind::Cpu,
                        name: "test".into(),
                        properties: Vec::new(),
                    },
                    sample_number: 0,
                    gate: receiver,
                }))
            },
        )
        .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(worker.take().is_none()); // sampling is still blocked by the gate
        gate.send(()).unwrap();
        updated.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            worker.take().unwrap().unwrap().interval,
            Some(Duration::from_secs(1))
        );
        worker.stop.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(worker.is_finished());
    }

    #[test]
    fn discovery_error_is_reported_to_the_window() {
        let (notify, updated) = mpsc::channel();
        let worker = Worker::start(
            Duration::from_secs(1),
            move || {
                let _ = notify.send(());
            },
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fixture access denied",
                )
                .into())
            },
        )
        .unwrap();
        updated.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(worker.take().unwrap().unwrap_err(), "fixture access denied");
    }
}
