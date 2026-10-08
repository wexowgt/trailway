//! Runs deploy jobs from the API through the `Runtime` and keeps a record of
//! every deployment on this host, so the actual state can be reported after a
//! reconnect or an agent restart.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use trailway_proto::{
    AgentMessage, ApiMessage, DeployJob, DeploymentReport, DeploymentStatus, VmState,
};
use uuid::Uuid;

use crate::runtime::Runtime;

/// Finished deployments kept in the record (oldest dropped first).
const KEEP_FINISHED: usize = 100;
const LOG_CHUNK: usize = 64 * 1024;
const LOG_POLL: Duration = Duration::from_millis(500);

pub type SharedRuntime = Arc<dyn Runtime + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    deployment_id: Uuid,
    service_id: Uuid,
    status: DeploymentStatus,
    vm_id: Option<String>,
    host_port: Option<u16>,
    error: Option<String>,
}

impl Entry {
    fn report(&self) -> DeploymentReport {
        DeploymentReport {
            deployment_id: self.deployment_id,
            status: self.status,
            vm_id: self.vm_id.clone(),
            host_port: self.host_port,
            error: self.error.clone(),
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
    pub fn new(rt: SharedRuntime, path: Option<PathBuf>) -> Arc<Self> {
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
            path,
            entries: Mutex::new(entries),
            sink: Mutex::new(None),
            jobs,
        });
        manager.persist();
        tokio::spawn(worker(manager.clone(), rx));
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
            });
        }
        let _ = self.jobs.send(msg);
    }

    /// Starts talking to a freshly connected API: reports the actual state of
    /// every deployment, then streams the logs of the running ones.
    pub fn attach(self: &Arc<Self>, sink: UnboundedSender<AgentMessage>) {
        self.refresh();
        let _ = sink.send(AgentMessage::Hello {
            deployments: self.reports(),
        });
        *self.sink.lock().unwrap() = Some(sink.clone());
        for e in self.entries() {
            if let (DeploymentStatus::Running, Some(vm_id)) = (e.status, e.vm_id) {
                tokio::spawn(follow_logs(
                    self.rt.clone(),
                    e.deployment_id,
                    vm_id,
                    sink.clone(),
                ));
            }
        }
    }

    pub fn detach(&self) {
        *self.sink.lock().unwrap() = None;
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
        if let Err(e) = self.rt.prepare(&job.spec) {
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
        let vm_id = match self.rt.start(&job.spec) {
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
            tokio::spawn(follow_logs(self.rt.clone(), id, vm_id, sink));
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

/// Sends a deployment's console output to the API, from the start, until the
/// VM is gone and the log is drained or the connection ends.
async fn follow_logs(
    rt: SharedRuntime,
    deployment_id: Uuid,
    vm_id: String,
    sink: UnboundedSender<AgentMessage>,
) {
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
                offset,
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
    use crate::runtime::{FakeRuntime, FAIL_IMAGE_PREFIX};
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
        }
    }

    fn manager(rt: Arc<FakeRuntime>, path: Option<PathBuf>) -> Arc<Manager> {
        Manager::new(rt, path)
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

    #[test]
    fn splits_at_the_last_newline() {
        assert_eq!(complete_lines(b"a\nb", false), 2);
        assert_eq!(complete_lines(b"a\nb", true), 2);
        assert_eq!(complete_lines(b"abc", false), 0);
        assert_eq!(complete_lines(b"abc", true), 3);
    }
}
