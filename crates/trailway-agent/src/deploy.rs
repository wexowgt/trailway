//! Runs deploy jobs from the API through the `Runtime` and keeps a record of
//! every deployment on this host, so the actual state can be reported after a
//! reconnect or an agent restart.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use trailway_proto::{
    AgentMessage, ApiMessage, DeployJob, DeploymentReport, DeploymentStatus, VmState,
};
use uuid::Uuid;

use crate::{
    build::Builder,
    proxy::{Proxy, Route},
    runtime::Runtime,
};

/// Build output kept per deployment (bytes); the rest of a huge log is dropped.
const MAX_BUILD_LOG: u64 = 8 * 1024 * 1024;
/// Finished deployments kept in the record (oldest dropped first).
const KEEP_FINISHED: usize = 100;
const LOG_CHUNK: usize = 64 * 1024;
const LOG_POLL: Duration = Duration::from_millis(500);
/// How often the proxy is made to match the running deployments again (Caddy
/// may have restarted, or been missing when a deploy finished).
const ROUTE_SYNC_INTERVAL: Duration = Duration::from_secs(30);

pub type SharedRuntime = Arc<dyn Runtime + Send + Sync>;
pub type SharedBuilder = Arc<dyn Builder + Send + Sync>;
pub type SharedProxy = Arc<dyn Proxy + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    deployment_id: Uuid,
    service_id: Uuid,
    status: DeploymentStatus,
    vm_id: Option<String>,
    host_port: Option<u16>,
    error: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    /// Public host name routed to `host_port` while running.
    #[serde(default)]
    domain: Option<String>,
}

impl Entry {
    fn report(&self) -> DeploymentReport {
        DeploymentReport {
            deployment_id: self.deployment_id,
            status: self.status,
            vm_id: self.vm_id.clone(),
            host_port: self.host_port,
            error: self.error.clone(),
            commit: self.commit.clone(),
        }
    }

    /// Holds (or is about to hold) a VM that has to be stopped to replace it.
    fn holds_vm(&self) -> bool {
        self.vm_id.is_some()
            && matches!(
                self.status,
                DeploymentStatus::Building
                    | DeploymentStatus::Deploying
                    | DeploymentStatus::Running
            )
    }
}

pub struct Manager {
    rt: SharedRuntime,
    builder: SharedBuilder,
    /// One file per git deployment with its build output; the VM's console
    /// log continues it in the log the API shows.
    log_dir: PathBuf,
    /// Held while build output is appended and sent, and while a fresh
    /// connection catches up on it, so no chunk is sent out of order.
    log_lock: Mutex<()>,
    proxy: SharedProxy,
    /// The last proxy error, so a Caddy that stays down is logged once.
    proxy_error: Mutex<Option<String>>,
    path: Option<PathBuf>,
    entries: Mutex<Vec<Entry>>,
    /// The live API connection, if any. Messages sent while there is none are
    /// dropped: the next `attach` reports the full state again.
    sink: Mutex<Option<UnboundedSender<AgentMessage>>>,
    jobs: UnboundedSender<ApiMessage>,
}

impl Manager {
    /// Loads the record from `path` (when given) and starts the job worker.
    /// Must be called inside a tokio runtime.
    pub fn new(
        rt: SharedRuntime,
        builder: SharedBuilder,
        proxy: SharedProxy,
        path: Option<PathBuf>,
    ) -> Arc<Self> {
        let log_dir = match path.as_ref().and_then(|p| p.parent()) {
            Some(dir) => dir.join("build-logs"),
            None => std::env::temp_dir().join(format!("tw-build-logs-{}", Uuid::new_v4())),
        };
        let mut entries: Vec<Entry> = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        // A job that died with the agent has no VM to report; the API hands it over again.
        entries.retain(|e| {
            e.vm_id.is_some()
                || matches!(
                    e.status,
                    DeploymentStatus::Failed | DeploymentStatus::Stopped
                )
        });
        let (jobs, rx) = unbounded_channel();
        let manager = Arc::new(Self {
            rt,
            builder,
            log_dir,
            log_lock: Mutex::new(()),
            proxy,
            proxy_error: Mutex::new(None),
            path,
            entries: Mutex::new(entries),
            sink: Mutex::new(None),
            jobs,
        });
        manager.persist();
        tokio::spawn(worker(manager.clone(), rx));
        tokio::spawn(route_sync(manager.clone()));
        manager
    }

    /// Takes a message from the API. Deploys and stops run one at a time.
    pub fn submit(&self, msg: ApiMessage) {
        if let ApiMessage::Deploy(job) = &msg {
            if let Some(known) = self.entry(job.deployment_id) {
                // Handed over twice (e.g. after a reconnect): just say where it is.
                self.emit(AgentMessage::Status(known.report()));
                return;
            }
            self.upsert(Entry {
                deployment_id: job.deployment_id,
                service_id: job.service_id,
                status: DeploymentStatus::Queued,
                vm_id: None,
                host_port: None,
                error: None,
                commit: None,
                domain: job.domain.clone(),
            });
        }
        let _ = self.jobs.send(msg);
    }

    /// Starts talking to a freshly connected API: reports the actual state of
    /// every deployment, then streams the logs of the running ones.
    pub fn attach(self: &Arc<Self>, sink: UnboundedSender<AgentMessage>) {
        self.refresh();
        let _guard = self.log_lock.lock().unwrap();
        let manager = self.clone();
        tokio::task::spawn_blocking(move || manager.sync_routes());
        let _ = sink.send(AgentMessage::Hello {
            deployments: self.reports(),
        });
        *self.sink.lock().unwrap() = Some(sink.clone());
        for e in self.entries() {
            match (e.status, e.vm_id) {
                (DeploymentStatus::Running, Some(vm_id)) => {
                    tokio::spawn(follow_logs(
                        self.rt.clone(),
                        e.deployment_id,
                        vm_id,
                        self.log_path(e.deployment_id),
                        sink.clone(),
                    ));
                }
                // Build output from while the API was not listening; chunks it
                // already has are dropped there.
                (
                    DeploymentStatus::Building
                    | DeploymentStatus::Deploying
                    | DeploymentStatus::Failed,
                    _,
                ) => {
                    send_log_file(&self.log_path(e.deployment_id), e.deployment_id, &sink);
                }
                _ => {}
            }
        }
    }

    pub fn detach(&self) {
        *self.sink.lock().unwrap() = None;
    }

    /// Makes the proxy serve the running deployments that have a domain and a
    /// forwarded port, and nothing else.
    pub fn sync_routes(&self) {
        let mut routes: Vec<Route> = vec![];
        for e in self.entries() {
            let (DeploymentStatus::Running, Some(domain), Some(port)) =
                (e.status, e.domain, e.host_port)
            else {
                continue;
            };
            let route = Route {
                key: e.service_id.to_string(),
                domain,
                port,
            };
            routes.retain(|r| r.key != route.key);
            routes.push(route);
        }
        let result = self.proxy.sync(&routes).map_err(|e| format!("{e:#}"));
        let mut last = self.proxy_error.lock().unwrap();
        match result {
            Ok(()) => *last = None,
            Err(e) => {
                if last.as_ref() != Some(&e) {
                    tracing::warn!("could not update the public routes: {e}");
                }
                *last = Some(e);
            }
        }
    }

    /// Marks running deployments whose VM is gone as failed.
    pub fn refresh(&self) {
        for e in self.entries() {
            let Some(vm_id) = e.vm_id.as_deref() else {
                continue;
            };
            if e.status != DeploymentStatus::Running {
                continue;
            }
            let alive = self
                .rt
                .status(vm_id)
                .is_ok_and(|info| info.state == VmState::Running);
            if !alive {
                self.update(e.deployment_id, |e| {
                    e.status = DeploymentStatus::Failed;
                    e.host_port = None;
                    e.error = Some("The VM exited".into());
                });
            }
        }
    }

    fn log_path(&self, id: Uuid) -> PathBuf {
        self.log_dir.join(format!("{id}.log"))
    }

    /// Adds build output to the deployment's log file and sends it on.
    fn append_log(&self, id: Uuid, text: &str) {
        use std::io::Write;
        let _guard = self.log_lock.lock().unwrap();
        let path = self.log_path(id);
        let written = (|| -> std::io::Result<u64> {
            std::fs::create_dir_all(&self.log_dir)?;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            let offset = file.metadata()?.len();
            if offset >= MAX_BUILD_LOG {
                return Ok(offset);
            }
            file.write_all(text.as_bytes())?;
            Ok(offset)
        })();
        match written {
            Ok(offset) if offset < MAX_BUILD_LOG => self.emit(AgentMessage::Logs {
                deployment_id: id,
                offset,
                len: text.len() as u64,
                text: text.to_string(),
            }),
            Ok(_) => {}
            Err(e) => tracing::error!(deployment = %id, "could not store build log: {e}"),
        }
    }

    fn reports(&self) -> Vec<DeploymentReport> {
        self.entries().iter().map(Entry::report).collect()
    }

    fn entries(&self) -> Vec<Entry> {
        self.entries.lock().unwrap().clone()
    }

    fn entry(&self, id: Uuid) -> Option<Entry> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.deployment_id == id)
            .cloned()
    }

    fn emit(&self, msg: AgentMessage) {
        if let Some(sink) = self.sink.lock().unwrap().as_ref() {
            let _ = sink.send(msg);
        }
    }

    fn persist(&self) {
        let Some(path) = &self.path else { return };
        let json = serde_json::to_vec_pretty(&*self.entries.lock().unwrap());
        let written = json.map_err(anyhow::Error::from).and_then(|json| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        });
        if let Err(e) = written {
            tracing::error!("could not store deployments: {e:#}");
        }
    }

    fn upsert(&self, entry: Entry) {
        let report = entry.report();
        {
            let mut entries = self.entries.lock().unwrap();
            match entries
                .iter_mut()
                .find(|e| e.deployment_id == entry.deployment_id)
            {
                Some(slot) => *slot = entry,
                None => entries.push(entry),
            }
            let finished = entries.iter().filter(|e| e.status.is_terminal()).count();
            for _ in KEEP_FINISHED..finished {
                if let Some(i) = entries.iter().position(|e| e.status.is_terminal()) {
                    entries.remove(i);
                }
            }
        }
        self.emit(AgentMessage::Status(report));
        self.persist();
    }

    /// Changes one entry, stores it and tells the API.
    fn update(&self, id: Uuid, change: impl FnOnce(&mut Entry)) {
        let Some(mut entry) = self.entry(id) else {
            return;
        };
        change(&mut entry);
        self.upsert(entry);
    }

    fn run(&self, msg: ApiMessage) {
        match msg {
            ApiMessage::Deploy(job) => self.run_deploy(&job),
            ApiMessage::Stop { deployment_id } => self.run_stop(deployment_id),
        }
        self.sync_routes();
    }

    fn run_deploy(&self, job: &DeployJob) {
        let id = job.deployment_id;
        // Stopped before its turn came.
        if self
            .entry(id)
            .is_none_or(|e| e.status != DeploymentStatus::Queued)
        {
            return;
        }
        let set = |status, error: Option<String>| {
            self.update(id, move |e| {
                e.status = status;
                e.error = error;
            })
        };
        set(DeploymentStatus::Building, None);
        let mut spec = job.spec.clone();
        if let Some(source) = &job.source {
            match self
                .builder
                .build(id, source, &mut |line| self.append_log(id, line))
            {
                Ok(built) => {
                    spec.image = built.image;
                    self.update(id, |e| e.commit = Some(built.commit));
                }
                Err(e) => {
                    self.append_log(id, &format!("Build failed: {e:#}\n"));
                    return set(
                        DeploymentStatus::Failed,
                        Some(format!("Build failed: {e:#}")),
                    );
                }
            }
        }
        if let Err(e) = self.rt.prepare(&spec) {
            if job.source.is_some() {
                self.append_log(id, &format!("Could not prepare the image: {e:#}\n"));
            }
            return set(DeploymentStatus::Failed, Some(format!("{e:#}")));
        }

        set(DeploymentStatus::Deploying, None);
        // Stop old, start new: the previous VM of this service goes first.
        for old in self
            .entries()
            .into_iter()
            .filter(|e| e.service_id == job.service_id && e.deployment_id != id && e.holds_vm())
        {
            self.stop_vm(&old);
        }
        let vm_id = match self.rt.start(&spec) {
            Ok(vm) => vm,
            Err(e) => return set(DeploymentStatus::Failed, Some(format!("{e:#}"))),
        };
        let host_port = self
            .rt
            .status(&vm_id)
            .ok()
            .and_then(|info| info.network)
            .and_then(|n| n.host_port);
        self.update(id, |e| {
            e.status = DeploymentStatus::Running;
            e.vm_id = Some(vm_id.clone());
            e.host_port = host_port;
        });
        if let Some(sink) = self.sink.lock().unwrap().clone() {
            tokio::spawn(follow_logs(
                self.rt.clone(),
                id,
                vm_id,
                self.log_path(id),
                sink,
            ));
        }
    }

    fn run_stop(&self, id: Uuid) {
        match self.entry(id) {
            Some(e) if e.status.is_terminal() => {
                self.emit(AgentMessage::Status(e.report()));
            }
            Some(e) => self.stop_vm(&e),
            // Unknown here: nothing runs, so tell the API it is gone.
            None => self.emit(AgentMessage::Status(DeploymentReport {
                deployment_id: id,
                status: DeploymentStatus::Stopped,
                vm_id: None,
                host_port: None,
                error: None,
                commit: None,
            })),
        }
    }

    /// Stops the entry's VM and records it as stopped. A VM that will not stop
    /// stays recorded as it was, so the API asks again.
    fn stop_vm(&self, e: &Entry) {
        if let Some(vm) = &e.vm_id {
            if let Err(err) = self.rt.stop(vm) {
                let gone = self.rt.status(vm).is_err();
                if !gone {
                    tracing::error!(deployment = %e.deployment_id, "stopping vm {vm} failed: {err:#}");
                    return;
                }
            }
        }
        self.update(e.deployment_id, |e| {
            e.status = DeploymentStatus::Stopped;
            e.host_port = None;
            e.error = None;
        });
    }
}

/// Keeps the proxy matching the running deployments.
async fn route_sync(manager: Arc<Manager>) {
    let mut ticker = tokio::time::interval(ROUTE_SYNC_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let m = manager.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || m.sync_routes()).await {
            tracing::error!("route sync panicked: {e}");
        }
    }
}

async fn worker(manager: Arc<Manager>, mut rx: UnboundedReceiver<ApiMessage>) {
    while let Some(msg) = rx.recv().await {
        let m = manager.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || m.run(msg)).await {
            tracing::error!("deploy job panicked: {e}");
        }
    }
}

/// Splits `buf` after its last newline; with no newline the whole buffer
/// counts when `flush` is set (the VM is gone or the line is huge).
fn complete_lines(buf: &[u8], flush: bool) -> usize {
    match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => i + 1,
        None if flush => buf.len(),
        None => 0,
    }
}

/// Sends the stored build output of a deployment, in chunks of whole lines.
/// Returns its size in bytes (0 when there is none).
fn send_log_file(path: &Path, deployment_id: Uuid, sink: &UnboundedSender<AgentMessage>) -> u64 {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let mut offset = 0usize;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        let window = &rest[..rest.len().min(LOG_CHUNK)];
        let take = complete_lines(window, true);
        let text = String::from_utf8_lossy(&window[..take]).into_owned();
        let msg = AgentMessage::Logs {
            deployment_id,
            offset: offset as u64,
            len: take as u64,
            text,
        };
        if sink.send(msg).is_err() {
            break;
        }
        offset += take;
    }
    bytes.len() as u64
}

/// Sends a deployment's log to the API, from the start, until the VM is gone
/// and the log is drained or the connection ends: the build output first (if
/// it was built from git), then the VM's console after it.
async fn follow_logs(
    rt: SharedRuntime,
    deployment_id: Uuid,
    vm_id: String,
    build_log: PathBuf,
    sink: UnboundedSender<AgentMessage>,
) {
    let base = {
        let sink = sink.clone();
        tokio::task::spawn_blocking(move || {
            // Not while a build line is being appended and sent.
            send_log_file(&build_log, deployment_id, &sink)
        })
        .await
        .unwrap_or(0)
    };
    let mut offset = 0u64;
    loop {
        if sink.is_closed() {
            return;
        }
        let read = {
            let (rt, vm_id) = (rt.clone(), vm_id.clone());
            tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<u8>, bool)> {
                use std::io::Read;
                // Check before reading so output written just before exit is not lost.
                let running = rt
                    .status(&vm_id)
                    .is_ok_and(|info| info.state == VmState::Running);
                let mut buf = Vec::new();
                rt.logs_from(&vm_id, offset)?
                    .take(LOG_CHUNK as u64)
                    .read_to_end(&mut buf)?;
                Ok((buf, running))
            })
            .await
        };
        let (buf, running) = match read {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::debug!(%deployment_id, "log read ended: {e:#}");
                return;
            }
            Err(_) => return,
        };
        let take = complete_lines(&buf, !running || buf.len() >= LOG_CHUNK);
        if take > 0 {
            let msg = AgentMessage::Logs {
                deployment_id,
                offset: base + offset,
                len: take as u64,
                text: String::from_utf8_lossy(&buf[..take]).into_owned(),
            };
            if sink.send(msg).is_err() {
                return;
            }
            offset += take as u64;
        } else if !running {
            return;
        } else {
            tokio::time::sleep(LOG_POLL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::FakeBuilder;
    use crate::{
        proxy::RecordingProxy,
        runtime::{FakeRuntime, FAIL_IMAGE_PREFIX},
    };
    use trailway_proto::GitSource;
    use trailway_proto::VmSpec;

    fn spec(image: &str) -> VmSpec {
        VmSpec {
            image: image.into(),
            vcpus: 1,
            mem_mib: 128,
            env: vec![],
            cmd: vec![],
            port: Some(80),
            host_port: None,
        }
    }

    fn job(service: Uuid, image: &str) -> DeployJob {
        DeployJob {
            deployment_id: Uuid::new_v4(),
            service_id: service,
            spec: spec(image),
            source: None,
            domain: None,
        }
    }

    fn manager(rt: Arc<FakeRuntime>, path: Option<PathBuf>) -> Arc<Manager> {
        Manager::new(
            rt,
            Arc::new(FakeBuilder::default()),
            Arc::new(RecordingProxy::default()),
            path,
        )
    }

    fn git_job(service: Uuid, url: &str) -> DeployJob {
        DeployJob {
            source: Some(GitSource {
                url: url.into(),
                branch: "main".into(),
            }),
            ..job(service, "")
        }
    }

    fn routed_manager(proxy: Arc<RecordingProxy>) -> Arc<Manager> {
        Manager::new(
            Arc::new(FakeRuntime::default()),
            Arc::new(FakeBuilder::default()),
            proxy,
            None,
        )
    }

    fn routes(proxy: &RecordingProxy) -> Vec<(String, u16)> {
        proxy
            .routes
            .lock()
            .unwrap()
            .iter()
            .map(|r| (r.domain.clone(), r.port))
            .collect()
    }

    fn statuses(m: &Manager) -> Vec<(Uuid, DeploymentStatus)> {
        m.entries()
            .iter()
            .map(|e| (e.deployment_id, e.status))
            .collect()
    }

    async fn settle(m: &Arc<Manager>) {
        for _ in 0..200 {
            let busy = m.entries().iter().any(|e| {
                matches!(
                    e.status,
                    DeploymentStatus::Queued
                        | DeploymentStatus::Building
                        | DeploymentStatus::Deploying
                )
            });
            if !busy {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("deploy did not settle: {:?}", statuses(m));
    }

    #[tokio::test]
    async fn deploy_runs_and_redeploy_replaces_the_vm() {
        let rt = Arc::new(FakeRuntime::default());
        let m = manager(rt.clone(), None);
        let service = Uuid::new_v4();
        let (tx, mut rx) = unbounded_channel();
        m.attach(tx);

        let first = job(service, "nginxdemos/hello");
        m.submit(ApiMessage::Deploy(first.clone()));
        settle(&m).await;
        let e1 = m.entry(first.deployment_id).unwrap();
        assert_eq!(e1.status, DeploymentStatus::Running);
        assert_eq!(e1.host_port, Some(30000));

        let second = job(service, "nginxdemos/hello");
        m.submit(ApiMessage::Deploy(second.clone()));
        settle(&m).await;
        assert_eq!(
            m.entry(first.deployment_id).unwrap().status,
            DeploymentStatus::Stopped
        );
        let e2 = m.entry(second.deployment_id).unwrap();
        assert_eq!(e2.status, DeploymentStatus::Running);
        assert_ne!(e1.vm_id, e2.vm_id);
        assert_eq!(
            rt.status(e1.vm_id.as_deref().unwrap()).unwrap().state,
            VmState::Stopped
        );

        // The first message is the Hello, then the status changes.
        assert!(matches!(rx.recv().await, Some(AgentMessage::Hello { .. })));
        let mut seen = vec![];
        while let Ok(msg) = rx.try_recv() {
            if let AgentMessage::Status(r) = msg {
                if r.deployment_id == second.deployment_id {
                    seen.push(r.status);
                }
            }
        }
        assert_eq!(
            seen,
            [
                DeploymentStatus::Queued,
                DeploymentStatus::Building,
                DeploymentStatus::Deploying,
                DeploymentStatus::Running
            ]
        );
    }

    #[tokio::test]
    async fn running_service_is_routed_and_stop_removes_the_route() {
        let proxy = Arc::new(RecordingProxy::default());
        let m = routed_manager(proxy.clone());
        let service = Uuid::new_v4();
        let domain = "hello-production.1-2-3-4.sslip.io".to_string();

        let mut first = job(service, "nginxdemos/hello");
        first.domain = Some(domain.clone());
        m.submit(ApiMessage::Deploy(first.clone()));
        settle(&m).await;
        assert_eq!(routes(&proxy), [(domain.clone(), 30000)]);

        // A redeploy keeps the domain and follows the new VM's port.
        let mut second = job(service, "nginxdemos/hello");
        second.domain = Some(domain.clone());
        m.submit(ApiMessage::Deploy(second.clone()));
        settle(&m).await;
        assert_eq!(routes(&proxy), [(domain, 30001)]);

        m.submit(ApiMessage::Stop {
            deployment_id: second.deployment_id,
        });
        for _ in 0..200 {
            if routes(&proxy).is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("route was not removed");
    }

    #[tokio::test]
    async fn services_without_domain_or_port_get_no_route() {
        let proxy = Arc::new(RecordingProxy::default());
        let m = routed_manager(proxy.clone());
        let no_domain = job(Uuid::new_v4(), "nginx");
        let mut no_port = job(Uuid::new_v4(), "worker");
        no_port.domain = Some("worker.example.com".into());
        no_port.spec.port = None;
        m.submit(ApiMessage::Deploy(no_domain));
        m.submit(ApiMessage::Deploy(no_port));
        settle(&m).await;
        m.sync_routes();
        assert!(routes(&proxy).is_empty());
    }

    #[tokio::test]
    async fn failed_prepare_is_reported_with_the_reason() {
        let m = manager(Arc::new(FakeRuntime::default()), None);
        let j = job(Uuid::new_v4(), &format!("{FAIL_IMAGE_PREFIX}app"));
        m.submit(ApiMessage::Deploy(j.clone()));
        settle(&m).await;
        let e = m.entry(j.deployment_id).unwrap();
        assert_eq!(e.status, DeploymentStatus::Failed);
        assert!(e.error.unwrap().contains("pull access denied"));
    }

    #[tokio::test]
    async fn stop_removes_the_vm_and_unknown_stops_are_reported() {
        let rt = Arc::new(FakeRuntime::default());
        let m = manager(rt.clone(), None);
        let j = job(Uuid::new_v4(), "nginx");
        m.submit(ApiMessage::Deploy(j.clone()));
        settle(&m).await;
        let vm = m.entry(j.deployment_id).unwrap().vm_id.unwrap();
        m.submit(ApiMessage::Stop {
            deployment_id: j.deployment_id,
        });
        for _ in 0..200 {
            if m.entry(j.deployment_id).unwrap().status == DeploymentStatus::Stopped {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(rt.status(&vm).unwrap().state, VmState::Stopped);

        let (tx, mut rx) = unbounded_channel();
        m.attach(tx);
        let ghost = Uuid::new_v4();
        m.submit(ApiMessage::Stop {
            deployment_id: ghost,
        });
        loop {
            if let Some(AgentMessage::Status(r)) = rx.recv().await {
                if r.deployment_id == ghost {
                    assert_eq!(r.status, DeploymentStatus::Stopped);
                    break;
                }
            }
        }
    }

    #[tokio::test]
    async fn duplicate_deploy_is_ignored_and_state_survives_restart() {
        let dir = std::env::temp_dir().join(format!("tw-agent-{}", Uuid::new_v4()));
        let path = dir.join("deployments.json");
        let rt = Arc::new(FakeRuntime::default());
        let m = manager(rt.clone(), Some(path.clone()));
        let j = job(Uuid::new_v4(), "nginx");
        m.submit(ApiMessage::Deploy(j.clone()));
        settle(&m).await;
        m.submit(ApiMessage::Deploy(j.clone()));
        assert_eq!(m.entries().len(), 1);

        // A new agent process reads the record; a dead VM shows as failed.
        let m2 = manager(rt.clone(), Some(path));
        assert_eq!(
            m2.entry(j.deployment_id).unwrap().status,
            DeploymentStatus::Running
        );
        rt.stop(m2.entry(j.deployment_id).unwrap().vm_id.as_deref().unwrap())
            .unwrap();
        m2.refresh();
        let e = m2.entry(j.deployment_id).unwrap();
        assert_eq!(e.status, DeploymentStatus::Failed);
        assert_eq!(e.error.as_deref(), Some("The VM exited"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn logs_stream_from_the_start() {
        let rt = Arc::new(FakeRuntime::default());
        let m = manager(rt, None);
        let (tx, mut rx) = unbounded_channel();
        m.attach(tx);
        let j = job(Uuid::new_v4(), "nginx");
        m.submit(ApiMessage::Deploy(j.clone()));
        loop {
            if let Some(AgentMessage::Logs {
                offset, len, text, ..
            }) = rx.recv().await
            {
                assert_eq!(offset, 0);
                assert_eq!(len as usize, text.len());
                assert!(text.contains("fake log"));
                break;
            }
        }
    }

    #[tokio::test]
    async fn git_deploy_builds_then_runs_the_built_image_with_one_log() {
        let rt = Arc::new(FakeRuntime::default());
        let m = manager(rt.clone(), None);
        let (tx, mut rx) = unbounded_channel();
        m.attach(tx);
        let j = git_job(Uuid::new_v4(), "https://github.com/o/app");
        m.submit(ApiMessage::Deploy(j.clone()));
        settle(&m).await;
        let e = m.entry(j.deployment_id).unwrap();
        assert_eq!(e.status, DeploymentStatus::Running);
        assert!(e.commit.is_some());
        let vm = rt.status(e.vm_id.as_deref().unwrap()).unwrap();
        assert!(vm.spec.image.starts_with("docker-daemon:trailway-build/"));

        // Build output first, then the VM's console, with contiguous offsets.
        let mut log = String::new();
        let mut statuses = vec![];
        while log.matches("fake log").count() == 0 {
            match rx.recv().await.unwrap() {
                AgentMessage::Logs {
                    offset, len, text, ..
                } => {
                    assert_eq!(len as usize, text.len());
                    // The build log is sent live and again with the VM's log:
                    // repeats are fine (the API drops them), gaps are not.
                    assert!(offset as usize <= log.len(), "gap at {offset}");
                    if offset as usize == log.len() {
                        log.push_str(&text);
                    }
                }
                AgentMessage::Status(r) => statuses.push(r.status),
                AgentMessage::Hello { .. } => {}
            }
        }
        assert!(log.starts_with("Cloning\nBuilding\n"), "{log}");
        assert!(statuses.contains(&DeploymentStatus::Building));
    }

    #[tokio::test]
    async fn failed_build_fails_the_deployment_and_keeps_the_old_vm() {
        let rt = Arc::new(FakeRuntime::default());
        let m = manager(rt.clone(), None);
        let (tx, mut rx) = unbounded_channel();
        m.attach(tx);
        let service = Uuid::new_v4();
        let ok = git_job(service, "https://github.com/o/app");
        m.submit(ApiMessage::Deploy(ok.clone()));
        settle(&m).await;
        let old_vm = m.entry(ok.deployment_id).unwrap().vm_id.unwrap();

        let bad = git_job(service, "https://github.com/o/broken");
        m.submit(ApiMessage::Deploy(bad.clone()));
        settle(&m).await;
        let e = m.entry(bad.deployment_id).unwrap();
        assert_eq!(e.status, DeploymentStatus::Failed);
        assert!(e.error.unwrap().contains("Build failed"));
        assert_eq!(rt.status(&old_vm).unwrap().state, VmState::Running);
        assert_eq!(
            m.entry(ok.deployment_id).unwrap().status,
            DeploymentStatus::Running
        );

        let mut log = String::new();
        while let Ok(msg) = rx.try_recv() {
            if let AgentMessage::Logs {
                deployment_id,
                text,
                ..
            } = msg
            {
                if deployment_id == bad.deployment_id {
                    log.push_str(&text);
                }
            }
        }
        assert!(log.contains("the build exploded") && log.contains("Build failed"));
    }

    #[tokio::test]
    async fn reconnect_resends_the_build_log_from_the_start() {
        let m = manager(Arc::new(FakeRuntime::default()), None);
        let bad = git_job(Uuid::new_v4(), "https://github.com/o/broken");
        m.submit(ApiMessage::Deploy(bad.clone()));
        settle(&m).await;
        let (tx, mut rx) = unbounded_channel();
        m.attach(tx);
        let mut text = String::new();
        while let Ok(msg) = rx.try_recv() {
            if let AgentMessage::Logs {
                offset, text: t, ..
            } = msg
            {
                assert_eq!(offset as usize, text.len());
                text.push_str(&t);
            }
        }
        assert!(text.starts_with("Cloning\n") && text.contains("Build failed"));
    }

    #[test]
    fn splits_at_the_last_newline() {
        assert_eq!(complete_lines(b"a\nb", false), 2);
        assert_eq!(complete_lines(b"a\nb", true), 2);
        assert_eq!(complete_lines(b"abc", false), 0);
        assert_eq!(complete_lines(b"abc", true), 3);
    }
}
