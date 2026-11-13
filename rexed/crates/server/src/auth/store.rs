//! Private, atomic on-disk user policy; authentication loads individual users on cache misses.
use anyhow::{anyhow, ensure, Context, Result};
use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHasher,
};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Permissions {
    pub execute: bool,
    pub files_read: bool,
    pub files_write: bool,
    pub pty_view: bool,
    pub pty_control: bool,
    pub forward: bool,
    pub management: bool,
}
impl Permissions {
    pub fn all() -> Self {
        Self {
            execute: true,
            files_read: true,
            files_write: true,
            pty_view: true,
            pty_control: true,
            forward: true,
            management: true,
        }
    }
    pub fn allows_tool(&self, method: &str, params: &serde_json::Value) -> bool {
        match method {
            "_executor_info" | "client_resources" => true,
            "stat" | "read" | "glob" | "rg" => self.files_read,
            "FileAction" => self.files_write,
            "exbash"
                if params["io"] == "stdio"
                    || params["asyncID"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("stdio-")) =>
            {
                self.execute && (params["filePath"].is_null() || self.files_read)
            }
            "exbash" => match params.get("mode").and_then(serde_json::Value::as_str) {
                Some("list") => self.execute || self.pty_view,
                Some("detail" | "status") => self.pty_view,
                Some("attach") => {
                    self.pty_view
                        && (params
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(str::is_empty)
                            || self.pty_control)
                        && (params
                            .get("filePath")
                            .is_none_or(serde_json::Value::is_null)
                            || (self.pty_control && self.files_read))
                }
                _ => self.execute,
            },
            _ => false,
        }
    }
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct UserConfig {
    pub disabled: bool,
    pub password_hash: Option<String>,
    /// Paths relative to this user's directory; pin the complete leaf certificate.
    pub certificates: Vec<PathBuf>,
    pub certificate_authorities: Vec<PathBuf>,
    /// Required URI SAN for CA login; defaults to urn:rexed:user:<username>.
    pub ca_identity: Option<String>,
    pub permissions: Permissions,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerConfig {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    /// Independent operations listener; the standalone server CLI can override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_listen: Option<String>,
    #[serde(default)]
    pub logging: crate::events::LogConfig,
}
#[derive(Clone, Debug)]
pub struct ConfigDirectory {
    root: PathBuf,
}
impl ConfigDirectory {
    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let root = match path {
            Some(path) => path,
            None => std::env::current_exe()?
                .parent()
                .ok_or_else(|| anyhow!("executable has no parent"))?
                .join("rexed-config"),
        };
        private_directory(&root)?;
        private_directory(&root.join("users"))?;
        Ok(Self { root })
    }
    pub fn path(&self) -> &Path {
        &self.root
    }
    pub fn initialize(&self, names: Vec<String>) -> Result<()> {
        let path = self.root.join("server.json");
        if path.exists() {
            return Ok(());
        }
        // Do not replace an orphaned key/certificate after an interrupted initialization.
        ensure!(
            !self.root.join("server-key.pem").exists()
                && !self.root.join("server-cert.pem").exists(),
            "existing TLS files without server.json; configure server.json explicitly"
        );
        let names = if names.is_empty() {
            vec!["localhost".into(), "127.0.0.1".into(), "::1".into()]
        } else {
            names
        };
        let rcgen::CertifiedKey { cert, key_pair } = rcgen::generate_simple_self_signed(names)?;
        self.write("server-key.pem", key_pair.serialize_pem().as_bytes(), false)?;
        self.write("server-cert.pem", cert.pem().as_bytes(), false)?;
        self.write(
            "server.json",
            &serde_json::to_vec_pretty(&ServerConfig {
                certificate: "server-cert.pem".into(),
                private_key: "server-key.pem".into(),
                admin_listen: None,
                logging: Default::default(),
            })?,
            false,
        )
    }
    pub fn server(&self) -> Result<ServerConfig> {
        let mut config: ServerConfig =
            serde_json::from_slice(&std::fs::read(self.root.join("server.json"))?)?;
        config.certificate = self.root.join(config.certificate);
        config.private_key = self.root.join(config.private_key);
        Ok(config)
    }
    pub fn user_directory(&self, username: &str) -> Result<PathBuf> {
        validate_username(username)?;
        Ok(self.root.join("users").join(username))
    }
    pub fn save_user(&self, username: &str, config: &UserConfig) -> Result<()> {
        let directory = self.user_directory(username)?;
        private_directory(&directory)?;
        self.write(
            &format!("users/{username}/config.json"),
            &serde_json::to_vec_pretty(config)?,
            true,
        )
    }
    pub fn load_user(&self, username: &str) -> Result<UserConfig> {
        let directory = self.user_directory(username)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "user directory must be a real directory"
        );
        let path = directory.join("config.json");
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "user config must be a regular file"
        );
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }
    pub fn users(&self) -> Result<Vec<(String, UserConfig, PathBuf)>> {
        let mut users = Vec::new();
        for entry in std::fs::read_dir(self.root.join("users"))? {
            let entry = entry?;
            ensure!(
                !entry.file_type()?.is_symlink(),
                "user directories cannot be symlinks"
            );
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow!("invalid user directory"))?;
            users.push((
                name.clone(),
                self.load_user(&name)
                    .with_context(|| format!("load user {name}"))?,
                entry.path(),
            ));
        }
        Ok(users)
    }
    fn write(&self, name: &str, data: &[u8], overwrite: bool) -> Result<()> {
        let path = self.root.join(name);
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("configuration path has no parent"))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        private_file(temp.path())?;
        temp.write_all(data)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        if overwrite {
            temp.persist(&path)?;
        } else {
            temp.persist_noclobber(&path)?;
        }
        Ok(())
    }
}
pub fn validate_username(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            && !name.ends_with('.')
            && !matches!(
                name.to_ascii_lowercase().as_str(),
                "con"
                    | "prn"
                    | "aux"
                    | "nul"
                    | "com1"
                    | "com2"
                    | "com3"
                    | "com4"
                    | "com5"
                    | "com6"
                    | "com7"
                    | "com8"
                    | "com9"
                    | "lpt1"
                    | "lpt2"
                    | "lpt3"
                    | "lpt4"
                    | "lpt5"
                    | "lpt6"
                    | "lpt7"
                    | "lpt8"
                    | "lpt9"
            ),
        "username must be 1..64 ASCII letters, digits, '_' or '-', excluding reserved device names"
    );
    Ok(())
}
pub fn hash_password(password: &str) -> Result<String> {
    ensure!(
        !password.is_empty() && password.len() <= 1024,
        "password must contain 1..1024 bytes"
    );
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))?
        .to_string())
}
pub(crate) fn private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !path.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        ensure!(
            std::fs::symlink_metadata(path)?.is_dir(),
            "config directory must be a real directory"
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)?;
        ensure!(
            std::fs::symlink_metadata(path)?.is_dir(),
            "config directory must be a real directory"
        );
        private_file(path)?;
    }
    Ok(())
}
pub(crate) fn private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        // Use the process token's SID, not an environment-provided account name.
        let output = std::process::Command::new("whoami.exe")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()?;
        ensure!(output.status.success(), "cannot determine config owner SID");
        let text = String::from_utf8(output.stdout)?;
        let sid = text
            .trim()
            .split(',')
            .next_back()
            .unwrap_or_default()
            .trim_matches('"');
        ensure!(
            sid.starts_with("S-1-")
                && sid
                    .bytes()
                    .all(|c| c.is_ascii_digit() || matches!(c, b'S' | b'-')),
            "invalid config owner SID"
        );
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
                    SDDL_REVISION_1, SE_FILE_OBJECT,
                },
                GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION,
            },
        };
        let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        // Build a new protected DACL, removing both inherited and pre-existing broad grants.
        unsafe {
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = std::ptr::null_mut();
            if GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) == 0 {
                let error = std::io::Error::last_os_error();
                LocalFree(descriptor);
                return Err(error.into());
            }
            let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let status = SetNamedSecurityInfoW(
                name.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            );
            LocalFree(descriptor);
            if status != 0 {
                return Err(std::io::Error::from_raw_os_error(status as i32).into());
            }
        }
    }
    Ok(())
}
