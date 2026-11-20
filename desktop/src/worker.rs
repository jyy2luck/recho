use eframe::egui;
use recho_core::native::{DesktopEngine, Detail, Phase, Snapshot};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tokio::sync::mpsc;

pub enum Command {
    Port(u16),
    Configure {
        running: bool,
        intercept_tls: bool,
    },
    Detail {
        generation: u64,
        id: String,
        phase: Phase,
    },
    Clear,
}

#[derive(Default)]
pub struct Inbox {
    pub ready: Option<Result<PathBuf, String>>,
    pub snapshot: Option<Snapshot>,
    pub details: [Option<(u64, Result<Detail, String>)>; 2],
    pub error: Option<String>,
}
impl Inbox {
    fn snapshot(&mut self, mut next: Snapshot) {
        // Preserve an unread list update when the following tick only contains status.
        if next.rows.is_none() {
            next.rows = self.snapshot.as_mut().and_then(|old| old.rows.take());
        }
        self.snapshot = Some(next);
    }
}

pub struct Worker {
    pub inbox: Arc<Mutex<Inbox>>,
    commands: Option<mpsc::Sender<Command>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    pub fn start(directory: PathBuf, port: u16, ctx: egui::Context) -> Self {
        let (tx, mut rx) = mpsc::channel(8);
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let pending = inbox.clone();
        let thread = thread::spawn(move || {
            let publish = |update: &mut dyn FnMut(&mut Inbox)| {
                update(&mut pending.lock().unwrap());
                ctx.request_repaint();
            };
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    publish(&mut |inbox| inbox.ready = Some(Err(error.to_string())));
                    return;
                }
            };
            runtime.block_on(async {
                let engine = match DesktopEngine::start(directory, port).await {
                    Ok(engine) => engine,
                    Err(error) => {
                        publish(&mut |inbox| inbox.ready = Some(Err(error.clone())));
                        return;
                    }
                };
                publish(&mut |inbox| inbox.ready = Some(Ok(engine.ca_path())));
                let mut revision = None;
                let mut status = None;
                let mut tick = tokio::time::interval(Duration::from_millis(300));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = tick.tick() => {},
                        command = rx.recv() => {
                            let Some(command) = command else { break };
                            let result = match command {
                                Command::Configure { running, intercept_tls } => engine.configure(running, intercept_tls).await,
                                Command::Clear => engine.clear(),
                                Command::Port(port) => engine.set_port(port).await,
                                Command::Detail { generation, id, phase } => {
                                    let result = engine.detail(&id, phase);
                                    let mut inbox = pending.lock().unwrap();
                                    inbox.details[usize::from(phase == Phase::Response)] = Some((generation, result));
                                    drop(inbox);
                                    ctx.request_repaint();
                                    Ok(())
                                }
                            };
                            if let Err(error) = result {
                                pending.lock().unwrap().error = Some(error);
                                ctx.request_repaint();
                            }
                        }
                    }
                    let snapshot = engine.snapshot(revision).await;
                    let next_status = (snapshot.running, snapshot.port, snapshot.intercept_tls, snapshot.history_error.clone());
                    let changed = snapshot.rows.is_some() || status.as_ref() != Some(&next_status);
                    revision = Some(snapshot.revision);
                    status = Some(next_status);
                    if changed {
                        pending.lock().unwrap().snapshot(snapshot);
                        ctx.request_repaint();
                    }
                }
                engine.stop().await;
            });
            runtime.shutdown_timeout(Duration::from_secs(2));
        });
        Self {
            inbox,
            commands: Some(tx),
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .as_ref()
            .ok_or("Engine has stopped")?
            .try_send(command)
            .map_err(|error| format!("Engine is busy or stopped: {error}"))
    }

    pub fn stop(&mut self) {
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recho_core::native::Row;

    #[test]
    fn mailbox_preserves_unread_rows_and_replaces_them_on_clear() {
        let mut inbox = Inbox::default();
        let snapshot = |revision, rows| Snapshot {
            revision,
            rows,
            running: true,
            port: 1234,
            intercept_tls: false,
            history_error: None,
        };
        inbox.snapshot(snapshot(
            1,
            Some(vec![Row {
                id: "one".into(),
                method: "GET".into(),
                url: "http://localhost".into(),
                status: 200,
                duration_ms: 1,
                response_bytes: 0,
                host: "localhost".into(),
                application: "unknown".into(),
                server_ip: String::new(),
                protocol: "HTTP/1.1".into(),
                content_type: "text/plain".into(),
                error: None,
            }]),
        ));
        inbox.snapshot(snapshot(1, None));
        assert_eq!(
            inbox
                .snapshot
                .as_ref()
                .unwrap()
                .rows
                .as_ref()
                .unwrap()
                .len(),
            1
        );
        inbox.snapshot(snapshot(2, Some(vec![])));
        inbox.snapshot(snapshot(2, None));
        assert!(inbox.snapshot.unwrap().rows.unwrap().is_empty());
    }
}
