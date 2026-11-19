//! Discovery for CLI-owned hosts. IDs survive process exit, but never cross OS boots.
pub mod ipc;
mod private;
use anyhow::{ensure, Context, Result};
pub mod directory;
pub use private::{private_directory, private_file};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// Project directory for CLI preferences and saved entries.
pub const PROJECT_DIR: &str = ".rexeterm";
pub const BINDING_FILE: &str = "RexETermHostUUID.txt";
pub const ID_HEADER: &str = "x-rexeterm-service-id";
pub fn validate_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) && id != "0000000000000000",
        "service ID must be a nonzero UUID64 (16 hexadecimal characters)"
    );
    Ok(())
}
pub fn normalize_id(id: &str) -> Result<String> {
    let id = id.trim().to_ascii_lowercase();
    validate_id(&id)?;
    Ok(id)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRecord {
    pub id: String,
    pub boot_id: String,
    pub config_dir: PathBuf,
    pub local: bool,
    pub target: Option<String>,
    pub token_file: Option<PathBuf>,
    pub pid: Option<u32>,
}
#[derive(Clone, Debug)]
pub struct Registry {
    directory: PathBuf,
    boot_id: String,
}
impl Registry {
    pub fn open(base: Option<PathBuf>) -> Result<Self> {
        Self::for_boot(Self::base(base)?, boot_id()?, true)
    }
    /// Inspect an existing registry without creating directories or files.
    pub fn inspect(base: Option<PathBuf>) -> Result<Self> {
        Self::for_boot(Self::base(base)?, boot_id()?, false)
    }
    fn base(base: Option<PathBuf>) -> Result<PathBuf> {
        match base {
            Some(base) => Ok(base),
            None => default_directory(),
        }
    }
    fn for_boot(base: PathBuf, boot_id: String, create: bool) -> Result<Self> {
        ensure!(
            !boot_id.is_empty()
                && boot_id.len() <= 100
                && boot_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid OS boot ID"
        );
        if create {
            private_directory(&base)?;
        }
        let base = match std::fs::canonicalize(&base) {
            Ok(path) => path,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                std::path::absolute(base)?
            }
            Err(error) => return Err(error.into()),
        };
        let directory = base.join(&boot_id);
        if create {
            private_directory(&directory)?;
        }
        Ok(Self { directory, boot_id })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn base_directory(&self) -> &Path {
        self.directory.parent().expect("boot directory has parent")
    }
    fn lock(&self) -> Result<File> {
        let f = private_open(&self.directory.join("registry.lock"))?;
        f.lock()?;
        Ok(f)
    }
    fn path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
    pub fn records(&self) -> Result<Vec<ServiceRecord>> {
        let mut records = Vec::new();
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let Some(id) = path.file_stem().and_then(|v| v.to_str()) else {
                    continue;
                };
                if validate_id(id).is_ok() {
                    records.push(self.resolve(id)?);
                }
            }
        }
        records.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(records)
    }
    pub fn resolve(&self, id: &str) -> Result<ServiceRecord> {
        let id = normalize_id(id)?;
        let mut file = File::open(self.path(&id)).with_context(|| {
            format!("cannot find service {id} in {}; check --registry-dir and run service list or service create", self.directory.display())
        })?;
        ensure!(
            file.metadata()?.len() <= 65536,
            "service record exceeds 64 KiB"
        );
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let record: ServiceRecord = serde_json::from_slice(&bytes)?;
        ensure!(
            record.id == id && record.boot_id == self.boot_id,
            "service identity or OS boot mismatch"
        );
        Ok(record)
    }
    pub fn reserve(&self, config_dir: &Path, local: bool) -> Result<ServiceRecord> {
        private_directory(config_dir)?;
        let config_dir = std::fs::canonicalize(config_dir)?;
        let _lock = self.lock()?;
        if let Some(record) = self
            .records()?
            .into_iter()
            .find(|r| r.config_dir == config_dir)
        {
            ensure!(
                record.local == local,
                "host local-executor setting differs; use service start for the existing service"
            );
            return Ok(record);
        }
        loop {
            let id = format!("{:016x}", rand::random::<u64>());
            if id == "0000000000000000" || self.path(&id).exists() {
                continue;
            }
            let record = ServiceRecord {
                id,
                boot_id: self.boot_id.clone(),
                config_dir: config_dir.clone(),
                local,
                target: None,
                token_file: None,
                pid: None,
            };
            self.write(&record)?;
            return Ok(record);
        }
    }
    fn write(&self, record: &ServiceRecord) -> Result<()> {
        write_atomic(&self.path(&record.id), &serde_json::to_vec_pretty(record)?)
    }
    pub fn start(&self, id: &str) -> Result<Registration> {
        let record = self.resolve(id)?;
        let lock = private_open(&record.config_dir.join("cli-host.lock"))?;
        lock.try_lock()
            .context("this host config directory is already running")?;
        Ok(Registration {
            registry: self.clone(),
            record,
            _lock: lock,
            published: false,
        })
    }
}
/// The lock is held by the host process for its complete lifetime, not by a CLI invocation.
pub struct Registration {
    registry: Registry,
    record: ServiceRecord,
    _lock: File,
    published: bool,
}
impl Registration {
    pub fn record(&self) -> &ServiceRecord {
        &self.record
    }
    #[cfg(unix)]
    pub fn socket_path(&self) -> Result<PathBuf> {
        let path = self
            .registry
            .base_directory()
            .join(format!("{}.sock", self.record.id));
        Ok(path)
    }
    pub fn token_path(&self) -> PathBuf {
        self.registry
            .directory()
            .join(format!("{}.token", self.record.id))
    }
    #[cfg(windows)]
    pub fn pipe_name(&self) -> String {
        format!("rexeterm-{}", self.record.id)
    }
    pub fn publish(&mut self, target: String, token_file: Option<PathBuf>) -> Result<()> {
        self.record.target = Some(target);
        self.record.token_file = token_file;
        self.record.pid = Some(std::process::id());
        let _lock = self.registry.lock()?;
        self.registry.write(&self.record)?;
        self.published = true;
        Ok(())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if !self.published {
            return;
        }
        if let Ok(_lock) = self.registry.lock() {
            self.record.target = None;
            self.record.token_file = None;
            self.record.pid = None;
            let _ = self.registry.write(&self.record);
        }
    }
}
pub fn read_binding(cwd: &Path) -> Result<Option<(String, PathBuf)>> {
    for path in [
        cwd.join(BINDING_FILE),
        cwd.join(PROJECT_DIR).join("binding"),
    ] {
        let mut file = match File::open(&path) {
            Ok(file) => file.take(65),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("read binding {}", path.display()))
            }
        };
        let mut text = String::new();
        file.read_to_string(&mut text)
            .with_context(|| format!("read binding {}", path.display()))?;
        ensure!(
            text.len() <= 64,
            "CLI binding {} exceeds 64 bytes",
            path.display()
        );
        let id =
            normalize_id(&text).with_context(|| format!("invalid binding {}", path.display()))?;
        return Ok(Some((id, path)));
    }
    Ok(None)
}

pub fn write_binding(cwd: &Path, id: &str) -> Result<()> {
    let id = normalize_id(id)?;
    write_atomic(&cwd.join(BINDING_FILE), format!("{id}\n").as_bytes())
}
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("file needs a parent directory")?)?;
    private_file(file.path())?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
fn private_open(path: &Path) -> Result<File> {
    ensure!(
        !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()),
        "lock file must not be a symlink"
    );
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    private_file(path)?;
    Ok(file)
}
pub fn default_directory() -> Result<PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .context("CLI executable has no parent")?
        .join(".rexeterm-socket"))
}
/// The OS account is unchanged; this is only the Linux process display name.
pub fn set_daemon_name() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: PR_SET_NAME copies at most 16 bytes from this NUL-terminated string.
        let result = unsafe { libc::prctl(libc::PR_SET_NAME, c"rexetermd".as_ptr(), 0, 0, 0) };
        ensure!(
            result == 0,
            "cannot set daemon process name: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

fn boot_id() -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned())
    }
    #[cfg(windows)]
    {
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtQuerySystemInformation(
                class: u32,
                buffer: *mut std::ffi::c_void,
                length: u32,
                returned: *mut u32,
            ) -> i32;
        }
        let mut buffer = [0u64; 8];
        let mut returned = 0;
        // SAFETY: class 90 returns SYSTEM_BOOT_ENVIRONMENT_INFORMATION. The aligned,
        // writable buffer is larger than that structure, and returned is writable.
        let status =
            unsafe { NtQuerySystemInformation(90, buffer.as_mut_ptr().cast(), 64, &mut returned) };
        ensure!(
            status >= 0 && returned >= 16,
            "cannot query Windows boot identity ({status:#x})"
        );
        Ok(format!("{:016x}{:016x}", buffer[0], buffer[1]))
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()?;
        ensure!(output.status.success(), "cannot query OS boot identity");
        Ok(String::from_utf8(output.stdout)?
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect())
    }
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!("OS boot discovery is unsupported")
    }
}

/// Launch without inheriting the CLI terminal/session. Stdio must be configured by the caller.
pub fn spawn_detached(command: &mut std::process::Command) -> Result<std::process::Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and does not touch parent memory.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    Ok(command.spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_identity_survives_process_registration_but_never_crosses_boots() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let a = Registry::for_boot(root.path().join("runtime"), "boot-a".into(), true).unwrap();
        let record = a.reserve(&config, false).unwrap();
        let mut live = a.start(&record.id).unwrap();
        live.publish("unix:///example.sock".into(), None).unwrap();
        assert!(a.start(&record.id).is_err());
        drop(live);
        assert_eq!(a.reserve(&config, false).unwrap().id, record.id);
        assert!(a.resolve(&record.id).unwrap().target.is_none());
        let b = Registry::for_boot(root.path().join("runtime"), "boot-b".into(), true).unwrap();
        assert!(b.resolve(&record.id).is_err());
        assert_ne!(b.reserve(&config, false).unwrap().id, record.id);
    }
    #[test]
    fn native_boot_identity_is_stable() {
        assert_eq!(boot_id().unwrap(), boot_id().unwrap());
    }
    #[test]
    fn bindings_are_local_and_the_primary_takes_precedence() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path();
        let b = a.join("child");
        std::fs::create_dir_all(b.join(PROJECT_DIR)).unwrap();
        write_binding(a, "abcdef0123456789").unwrap();
        assert_eq!(read_binding(a).unwrap().unwrap().0, "abcdef0123456789");
        assert!(read_binding(&b).unwrap().is_none());
        let legacy = b.join(PROJECT_DIR).join("binding");
        std::fs::write(&legacy, "1111111111111111\n").unwrap();
        assert_eq!(
            read_binding(&b).unwrap(),
            Some(("1111111111111111".into(), legacy))
        );
        write_binding(&b, "2222222222222222").unwrap();
        assert_eq!(read_binding(&b).unwrap().unwrap().0, "2222222222222222");
        std::fs::write(b.join(BINDING_FILE), "invalid-id").unwrap();
        assert!(read_binding(&b).is_err());
        assert!(normalize_id("../../bad").is_err());
        assert!(normalize_id("0000000000000000").is_err());
    }
}
