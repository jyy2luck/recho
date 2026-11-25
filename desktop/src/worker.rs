use eframe::egui;
use recho_core::native::{
    CaptureBatch, CaptureMatches, ConditionSet, DesktopEngine, Detail, HistoryDeletePlan,
    HistoryDeletion, HistoryPage, Phase, Snapshot,
};
use serde_json::Value;
use std::{
    net::Ipv4Addr,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};

pub type RequestHandle = oneshot::Receiver<Result<Value, String>>;
pub type DetailHandle = oneshot::Receiver<Result<Detail, String>>;
pub type HistoryHandle = oneshot::Receiver<Result<HistoryPage, String>>;
pub type MatchHandle = oneshot::Receiver<Result<CaptureMatches, String>>;
pub type PortHandle = oneshot::Receiver<Result<u16, String>>;

#[derive(Clone, Copy)]
pub enum PortChange {
    Suggest(u16),
    Apply(u16),
}

pub enum Command {
    Port(PortChange, oneshot::Sender<Result<u16, String>>),
    Configure {
        running: bool,
        intercept_tls: bool,
    },
    Detail {
        generation: u64,
        id: String,
        phase: Phase,
    },
    Archive {
        group: String,
        ids: Option<Vec<String>>,
        close: bool,
    },
    PrepareHistoryDelete(HistoryDeletion),
    DeleteHistory(HistoryDeletePlan),
    ImportHar(PathBuf),
    ExportHar(PathBuf, Vec<String>),
    MatchCaptures {
        ids: Vec<String>,
        conditions: ConditionSet,
        highlights: ConditionSet,
        highlight: String,
        reply: oneshot::Sender<Result<CaptureMatches, String>>,
    },
    SystemProxy(bool),
    SystemProxyPreferences(crate::platform::ProxyPreferences),
    RestoreSystemProxy(oneshot::Sender<Result<(), String>>),
    Request {
        method: String,
        path: String,
        body: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    ArchivedDetail {
        id: String,
        phase: Phase,
        reply: oneshot::Sender<Result<Detail, String>>,
    },
    History {
        offset: usize,
        query: String,
        date: Option<String>,
        reply: oneshot::Sender<Result<HistoryPage, String>>,
    },
}

#[derive(Default)]
pub struct Inbox {
    pub ready: Option<Result<PathBuf, String>>,
    pub snapshot: Option<Snapshot>,
    pub captures: Option<CaptureBatch>,
    pub closed_captures: Vec<String>,
    pub history_changed: bool,
    pub history_delete_plan: Option<Result<HistoryDeletePlan, String>>,
    pub history_delete_finished: bool,
    pub details: [Option<(u64, Result<Detail, String>)>; 2],
    pub error: Option<String>,
    pub breakpoints: Option<Value>,
    pub workspace_operations: Option<Value>,
    pub proxy_host: Option<Ipv4Addr>,
    pub system_proxy: Option<(bool, bool)>,
    pub proxy_preferences: Option<crate::platform::ProxyPreferences>,
    pub imported: Option<(String, String, usize)>,
    pub notice: Option<String>,
}
impl Inbox {
    fn captures(&mut self, next: CaptureBatch) {
        if let Some(pending) = &mut self.captures
            && pending.group == next.group
            && !next.replace
        {
            pending.rows.extend(next.rows);
        } else {
            self.captures = Some(next);
        }
    }
}

pub struct Worker {
    pub inbox: Arc<Mutex<Inbox>>,
    commands: Option<mpsc::Sender<Command>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    pub fn start(directory: PathBuf, port: Option<u16>, ctx: egui::Context) -> Self {
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
                let mut system_proxy = crate::platform::ProxyOwner::new(&directory);
                let engine = match DesktopEngine::start(directory, port).await {
                    Ok(engine) => engine,
                    Err(error) => {
                        publish(&mut |inbox| inbox.ready = Some(Err(error.clone())));
                        return;
                    }
                };
                if let Err(error) = system_proxy.recover(&engine).await {
                    publish(&mut |inbox| inbox.error = Some(error.clone()));
                }
                let mut proxy_state = (system_proxy.active(), system_proxy.pending());
                let mut proxy_host = engine.proxy_host();
                publish(&mut |inbox| {
                    inbox.proxy_host = Some(proxy_host);
                    inbox.system_proxy = Some(proxy_state);
                    inbox.proxy_preferences = Some(system_proxy.preferences());
                });
                publish(&mut |inbox| inbox.ready = Some(Ok(engine.ca_path())));
                if system_proxy.preferences().auto {
                    let state = engine.status().await;
                    if state.running && let Err(error) = system_proxy.enable_automatically(&engine, state.port).await {
                        publish(&mut |inbox| inbox.error = Some(error.clone()));
                    }
                }
                let mut capture_group = String::new();
                let mut status = None;
                let mut breakpoints = Value::Null;
                let mut workspace_operations = Value::Null;
                let mut requests = JoinSet::new();
                let engine = Arc::new(engine);
                let mut tick = tokio::time::interval(Duration::from_millis(300));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let address_period = Duration::from_secs(5);
                let mut address_tick = tokio::time::interval_at(tokio::time::Instant::now() + address_period, address_period);
                address_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = tick.tick() => {},
                        _ = address_tick.tick() => {
                            let host = engine.proxy_host();
                            if host != proxy_host {
                                proxy_host = host;
                                publish(&mut |inbox| inbox.proxy_host = Some(host));
                            }
                        },
                        _ = requests.join_next(), if !requests.is_empty() => {},
                        command = rx.recv() => {
                            let Some(command) = command else { break };
                            let result = match command {
                                Command::Configure { running, intercept_tls } => {
                                    if !running && system_proxy.pending() {
                                        match system_proxy.restore(&engine).await {
                                            Ok(()) => engine.configure(running, intercept_tls).await,
                                            Err(error) => Err(error),
                                        }
                                    } else {
                                        let was_running = engine.status().await.running;
                                        match engine.configure(running, intercept_tls).await {
                                            Ok(()) if running && !was_running && system_proxy.preferences().auto => {
                                                let port = engine.status().await.port;
                                                system_proxy.enable_automatically(&engine, port).await
                                            }
                                            result => result,
                                        }
                                    }
                                },
                                Command::Archive { group, ids, close } => engine.archive_captures(&group, ids.as_deref()).map(|(saved, discarded)| {
                                    publish(&mut |inbox| {
                                        if close {
                                            inbox.closed_captures.push(group.clone());
                                        }
                                        inbox.history_changed = true;
                                        inbox.notice = Some(match (saved, discarded) {
                                            (0, 0) => "当前捕获已清空".into(),
                                            (0, n) => format!("已清空 {n} 条无痕记录"),
                                            (n, 0) => format!("已归档 {n} 条，按 F4 查看"),
                                            (n, m) => format!("已归档 {n} 条，按 F4 查看。已清空 {m} 条无痕记录"),
                                        });
                                    });
                                }),
                                Command::PrepareHistoryDelete(target) => {
                                    let mut result = Some(engine.prepare_history_delete(target));
                                    publish(&mut |inbox| inbox.history_delete_plan = result.take());
                                    Ok(())
                                }
                                Command::DeleteHistory(plan) => {
                                    let result = engine.delete_history(plan);
                                    publish(&mut |inbox| {
                                        inbox.history_changed = true;
                                        inbox.history_delete_finished = true;
                                        if let Ok(count) = &result {
                                            inbox.notice = Some(format!("已永久删除 {count} 条历史记录"));
                                        }
                                    });
                                    result.map(|_| ())
                                }
                                Command::ImportHar(path) => match engine.import_har(&path) {
                                    Ok((group, count)) => {
                                        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                                        publish(&mut |inbox| inbox.imported = Some((group.clone(), name.clone(), count)));
                                        Ok(())
                                    }
                                    Err(error) => Err(error),
                                },
                                Command::ExportHar(path, ids) => match engine.export_har(&path, &ids) {
                                    Ok(count) => {
                                        publish(&mut |inbox| inbox.notice = Some(format!("已导出 {count} 条 HAR 记录")));
                                        Ok(())
                                    }
                                    Err(error) => Err(error),
                                },
                                Command::MatchCaptures { ids, conditions, highlights, highlight, reply } => {
                                    if !reply.is_closed() {
                                        let _ = reply.send(engine.filter_captures(&ids, &conditions, &highlights, &highlight));
                                        ctx.request_repaint();
                                    }
                                    Ok(())
                                },
                                Command::Port(change, reply) => {
                                    let result = match change {
                                        PortChange::Suggest(previous) => engine.suggest_proxy_port(previous).await,
                                        PortChange::Apply(port) => {
                                            let enabled = system_proxy.active();
                                            match system_proxy.restore(&engine).await {
                                                Ok(()) => {
                                                    let mut result = engine.set_port(port).await;
                                                    let state = engine.status().await;
                                                    if enabled && state.running {
                                                        result = result.and(system_proxy.set(&engine, true, state.port).await);
                                                    }
                                                    result.map(|()| state.port)
                                                }
                                                Err(error) => Err(error),
                                            }
                                        }
                                    };
                                    let _ = reply.send(result);
                                    ctx.request_repaint();
                                    Ok(())
                                },
                                Command::SystemProxy(enabled) => {
                                    let state = engine.status().await;
                                    if enabled && !state.running {
                                        Err("请先启动调试".into())
                                    } else {
                                        let result = system_proxy.set(&engine, enabled, state.port).await;
                                        publish(&mut |inbox| inbox.proxy_preferences = Some(system_proxy.preferences()));
                                        result
                                    }
                                },
                                Command::SystemProxyPreferences(preferences) => {
                                    let result = system_proxy.set_preferences(&engine, preferences).await;
                                    publish(&mut |inbox| inbox.proxy_preferences = Some(system_proxy.preferences()));
                                    result
                                },
                                Command::RestoreSystemProxy(reply) => {
                                    let result = system_proxy.restore(&engine).await;
                                    proxy_state = (system_proxy.active(), system_proxy.pending());
                                    publish(&mut |inbox| inbox.system_proxy = Some(proxy_state));
                                    let _ = reply.send(result);
                                    ctx.request_repaint();
                                    Ok(())
                                },
                                Command::Request { method, path, body, mut reply } => {
                                    if requests.len() >= 8 {
                                        let _ = reply.send(Err("同时运行的操作过多，请稍后重试".into()));
                                        ctx.request_repaint();
                                    } else {
                                        let engine = engine.clone();
                                        let ctx = ctx.clone();
                                        requests.spawn(async move {
                                            tokio::select! {
                                                result = engine.request(&method, &path, body) => {
                                                    let _ = reply.send(result);
                                                    ctx.request_repaint();
                                                },
                                                _ = reply.closed() => {},
                                            }
                                        });
                                    }
                                    Ok(())
                                }
                                Command::ArchivedDetail { id, phase, reply } => {
                                    if !reply.is_closed() {
                                        let _ = reply.send(engine.detail(&id, phase));
                                        ctx.request_repaint();
                                    }
                                    Ok(())
                                }
                                Command::History { offset, query, date, reply } => {
                                    if !reply.is_closed() {
                                        let _ = reply.send(engine.desktop_history_page(date.as_deref(), offset, &query));
                                        ctx.request_repaint();
                                    }
                                    Ok(())
                                }
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
                    let snapshot = engine.status().await;
                    let next_proxy_state = (system_proxy.active(), system_proxy.pending());
                    if proxy_state != next_proxy_state {
                        proxy_state = next_proxy_state;
                        publish(&mut |inbox| inbox.system_proxy = Some(proxy_state));
                    }
                    let next_status = (snapshot.running, snapshot.port, snapshot.intercept_tls, snapshot.history_error.clone(), snapshot.group.clone(), snapshot.incognito, snapshot.enabled_list_filters.clone());
                    let changed = status.as_ref() != Some(&next_status);
                    status = Some(next_status);
                    let captures = engine.capture_batch(&snapshot.group, capture_group != snapshot.group);
                    if captures.is_ok() {
                        capture_group.clone_from(&snapshot.group);
                    }
                    if changed || !matches!(&captures, Ok(None)) {
                        let mut inbox = pending.lock().unwrap();
                        if changed {
                            inbox.snapshot = Some(snapshot);
                        }
                        match captures {
                            Ok(Some(captures)) => inbox.captures(captures),
                            Ok(None) => {}
                            Err(error) => inbox.error = Some(error),
                        }
                        drop(inbox);
                        ctx.request_repaint();
                    }
                    let next_breakpoints = engine.pending_breakpoints();
                    if next_breakpoints != breakpoints {
                        breakpoints = next_breakpoints;
                        pending.lock().unwrap().breakpoints = Some(breakpoints.clone());
                        ctx.request_repaint();
                    }
                    let operations = engine.pending_workspace_operations();
                    if operations != workspace_operations {
                        workspace_operations = operations;
                        publish(&mut |inbox| inbox.workspace_operations = Some(workspace_operations.clone()));
                    }
                }
                requests.shutdown().await;
                if let Err(error) = system_proxy.restore(&engine).await {
                    eprintln!("Could not restore the system proxy before exit: {error}");
                }
                if let Some(engine) = Arc::into_inner(engine) {
                    engine.stop().await;
                }
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

    pub fn request(
        &self,
        method: &str,
        path: impl Into<String>,
        body: Value,
    ) -> Result<RequestHandle, String> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Request {
            method: method.into(),
            path: path.into(),
            body,
            reply,
        })?;
        Ok(receive)
    }

    pub fn restore_system_proxy(&self) -> Result<oneshot::Receiver<Result<(), String>>, String> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::RestoreSystemProxy(reply))?;
        Ok(receive)
    }

    pub fn edit_port(&self, change: PortChange) -> Result<PortHandle, String> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Port(change, reply))?;
        Ok(receive)
    }

    pub fn detail(&self, id: String, phase: Phase) -> Result<DetailHandle, String> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::ArchivedDetail { id, phase, reply })?;
        Ok(receive)
    }

    pub fn filter_captures(
        &self,
        ids: Vec<String>,
        conditions: ConditionSet,
        highlights: ConditionSet,
        highlight: String,
    ) -> Result<MatchHandle, String> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::MatchCaptures {
            ids,
            conditions,
            highlights,
            highlight,
            reply,
        })?;
        Ok(receive)
    }

    pub fn history(
        &self,
        offset: usize,
        query: String,
        date: Option<String>,
    ) -> Result<HistoryHandle, String> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::History {
            offset,
            query,
            date,
            reply,
        })?;
        Ok(receive)
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
        let batch = |replace, rows| CaptureBatch {
            group: "default".into(),
            replace,
            rows,
        };
        let row = Row {
            id: "one".into(),
            method: "GET".into(),
            url: "http://localhost".into(),
            status: 200,
            duration_ms: 1,
            response_bytes: 0,
            host: "localhost".into(),
            application: "unknown".into(),
            application_path: None,
            server_ip: String::new(),
            protocol: "HTTP/1.1".into(),
            content_type: "text/plain".into(),
            websocket: false,
            binary: false,
            error: None,
            group: "default".into(),
            started_at: String::new(),
            incognito: false,
            list_matches: Default::default(),
        };
        let mut updated = row.clone();
        updated.status = 204;
        inbox.captures(batch(true, [(row.id.clone(), row)].into()));
        inbox.captures(batch(false, [(updated.id.clone(), updated)].into()));
        assert_eq!(inbox.captures.as_ref().unwrap().rows["one"].status, 204);
        assert!(inbox.captures.as_ref().unwrap().replace);
        inbox.captures(batch(true, Default::default()));
        assert!(inbox.captures.unwrap().rows.is_empty());
    }
}
