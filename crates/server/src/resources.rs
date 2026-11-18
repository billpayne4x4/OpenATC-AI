//! Bounded resource observations; unavailable counters remain unknown.
use serde_json::{Value, json};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
pub struct Monitor {
    system: System,
    sampled: bool,
}
impl Monitor {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            sampled: false,
        }
    }
    #[allow(clippy::cast_precision_loss)]
    pub fn sample(&mut self, root: Option<u32>) -> Value {
        self.system.refresh_memory();
        self.system.refresh_cpu_usage();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .without_tasks(),
        );
        let pids = descendants(&self.system, root);
        let mut memory = 0;
        let mut cpu = 0.0;
        for pid in &pids {
            if let Some(p) = self.system.process(sysinfo::Pid::from_u32(*pid)) {
                memory += p.memory();
                cpu += p.cpu_usage();
            }
        }
        let processes: Vec<_> = pids
            .iter()
            .filter_map(|pid| {
                self.system
                    .process(sysinfo::Pid::from_u32(*pid))
                    .map(|p| json!({"pid":pid,"ramBytes":p.memory()}))
            })
            .collect();
        let result = json!({"cpuName":self.system.cpus().first().map(sysinfo::Cpu::brand),"cores":self.system.cpus().len(),
            "cpuPercent": self.sampled.then(||self.system.global_cpu_usage()),
            "ramTotal":self.system.total_memory(),"ramUsed":self.system.used_memory(),
            "serverRam":root.map(|_|memory),"serverCpuPercent":(root.is_some()&&self.sampled).then(||cpu/self.system.cpus().len().max(1) as f32),"pids":pids,"processes":processes});
        self.sampled = true;
        result
    }
}
pub fn descendants(system: &System, root: Option<u32>) -> Vec<u32> {
    let Some(root) = root else { return vec![] };
    let mut result = vec![root];
    for _ in 0..16 {
        let before = result.len();
        for (pid, p) in system.processes() {
            if p.parent()
                .is_some_and(|parent| result.contains(&parent.as_u32()))
                && !result.contains(&pid.as_u32())
            {
                result.push(pid.as_u32());
            }
        }
        if result.len() == before {
            break;
        }
    }
    result
}
pub async fn gpu(pids: &[u32]) -> Value {
    let mut command = tokio::process::Command::new("nvidia-smi");
    command.args([
        "--query-gpu=index,name,memory.total,memory.used,utilization.gpu",
        "--format=csv,noheader,nounits",
    ]);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), command.output()).await;
    let Ok(Ok(output)) = output else {
        return json!({"available":false,"reason":"GPU counters unavailable on this driver/platform"});
    };
    if !output.status.success() {
        return json!({"available":false,"reason":"GPU counters unavailable on this driver/platform"});
    }
    let devices:Vec<_>=String::from_utf8_lossy(&output.stdout).lines().filter_map(|line|{
        let fields:Vec<_>=line.split(',').map(str::trim).collect();
        (fields.len()==5).then(||json!({"index":fields[0],"name":fields[1],"totalMiB":fields[2].parse::<u64>().ok(),"usedMiB":fields[3].parse::<u64>().ok(),"percent":fields[4].parse::<f32>().ok()}))
    }).collect();
    let mut command = tokio::process::Command::new("nvidia-smi");
    command
        .args([
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut used = None;
    let mut processes = vec![];
    if let Ok(Ok(output)) =
        tokio::time::timeout(std::time::Duration::from_secs(2), command.output()).await
        && output.status.success()
    {
        let mut total = 0;
        let mut found = false;
        let mut unknown = false;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let parts: Vec<_> = line.split(',').map(str::trim).collect();
            if parts.len() == 2 && parts[0].parse().is_ok_and(|pid| pids.contains(&pid)) {
                found = true;
                processes.push(json!({"pid":parts[0].parse::<u32>().ok(),"memoryMiB":parts[1].parse::<u64>().ok()}));
                if let Ok(value) = parts[1].parse::<u64>() {
                    total += value;
                } else {
                    unknown = true;
                }
            }
        }
        if found && !unknown {
            used = Some(total);
        }
    }
    json!({"available":!devices.is_empty(),"devices":devices,"serverMiB":used,"processes":processes,"reason":"VRAM counters are dedicated-device values; shared/unified memory may be unavailable"})
}
pub fn kill_descendants(root: u32) {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    for pid in descendants(&system, Some(root))
        .into_iter()
        .rev()
        .filter(|pid| *pid != root)
    {
        if let Some(p) = system.process(sysinfo::Pid::from_u32(pid)) {
            let _ = p.kill();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn process_memory_does_not_count_each_worker_thread() {
        let worker =
            std::thread::spawn(|| std::thread::park_timeout(std::time::Duration::from_secs(2)));
        let sample = super::Monitor::new().sample(Some(std::process::id()));
        let tasks: Vec<_> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|id| *id != std::process::id())
            .collect();
        let pids = sample["pids"].as_array().unwrap();
        assert!(!tasks.is_empty());
        assert!(pids.iter().all(|id| {
            u32::try_from(id.as_u64().unwrap_or(u64::MAX)).is_ok_and(|id| !tasks.contains(&id))
        }));
        worker.thread().unpark();
        worker.join().unwrap();
    }
}
