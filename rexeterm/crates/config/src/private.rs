use anyhow::{ensure, Result};
use std::path::Path;
pub fn private_directory(path: &Path) -> Result<()> {
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
pub fn private_file(path: &Path) -> Result<()> {
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
