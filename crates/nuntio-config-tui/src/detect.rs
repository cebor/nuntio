//! What is installed on this system, to offer in the pickers: shells for
//! `shell.program` and WSL distributions for `shell.wsl`.

/// A value to offer, with a short description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub value: String,
    pub help: String,
}

impl Found {
    fn new(value: impl Into<String>, help: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            help: help.into(),
        }
    }
}

/// Shells found on this system, most common first.
pub fn installed_shells() -> Vec<Found> {
    #[cfg(windows)]
    {
        // Only programs on PATH: alacritty doesn't quote the program, so a
        // path with spaces (like Git Bash's) wouldn't start reliably.
        // `bash` is left out, it's WSL's launcher on Windows.
        const KNOWN: [(&str, &str); 4] = [
            ("pwsh", "PowerShell 7"),
            ("powershell", "Windows PowerShell 5.1"),
            ("cmd", "Command Prompt"),
            ("nu", "Nushell"),
        ];
        KNOWN
            .iter()
            .filter(|(name, _)| on_path(&format!("{name}.exe")))
            .map(|(name, help)| Found::new(*name, *help))
            .collect()
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/etc/shells")
            .map(|text| {
                parse_etc_shells(&text)
                    .into_iter()
                    .filter(|path| std::path::Path::new(path).exists())
                    .map(|path| Found::new(path, ""))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(windows)]
fn on_path(file: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    // `symlink_metadata`: the Store's `pwsh.exe` is an app execution alias,
    // a reparse point that `metadata` can't follow.
    std::env::split_paths(&path).any(|dir| std::fs::symlink_metadata(dir.join(file)).is_ok())
}

/// The paths in `/etc/shells`, in order and without duplicates.
#[cfg_attr(windows, allow(dead_code))]
fn parse_etc_shells(text: &str) -> Vec<String> {
    let mut shells: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if !line.is_empty() && !shells.iter().any(|s| s == line) {
            shells.push(line.to_owned());
        }
    }
    shells
}

/// WSL distributions, the default one first. Empty outside Windows.
pub fn wsl_distributions() -> Vec<Found> {
    #[cfg(windows)]
    {
        let (distributions, default) = registry::distributions();
        order_distributions(distributions, default.as_deref())
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// `(id, name)` pairs as registered, and the default's id: shells can't run
/// in Docker Desktop's internal distributions, so those are left out.
#[cfg_attr(not(windows), allow(dead_code))]
fn order_distributions(distributions: Vec<(String, String)>, default: Option<&str>) -> Vec<Found> {
    let mut found: Vec<(bool, Found)> = distributions
        .into_iter()
        .filter(|(_, name)| !name.starts_with("docker-desktop"))
        .map(|(id, name)| {
            let is_default = default.is_some_and(|d| d.eq_ignore_ascii_case(&id));
            let help = if is_default {
                "The default distribution."
            } else {
                ""
            };
            (is_default, Found::new(name, help))
        })
        .collect();
    found.sort_by_key(|(is_default, found)| (!is_default, found.value.to_lowercase()));
    found.into_iter().map(|(_, found)| found).collect()
}

#[cfg(windows)]
mod registry {
    use std::ptr;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
        RegOpenKeyExW,
    };

    const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// A string value of `key`, or of its subkey `subkey`.
    fn string_value(key: HKEY, subkey: Option<&str>, name: &str) -> Option<String> {
        let subkey = subkey.map(wide);
        let subkey_ptr = subkey.as_ref().map_or(ptr::null(), |s| s.as_ptr());
        let name = wide(name);
        let mut buffer = [0u16; 512];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: all strings are NUL-terminated and `size` is the buffer's
        // size in bytes.
        let status = unsafe {
            RegGetValueW(
                key,
                subkey_ptr,
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        // `size` includes the terminating NUL.
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buffer[..len]))
    }

    /// Registered `(id, name)` pairs and the default distribution's id.
    pub fn distributions() -> (Vec<(String, String)>, Option<String>) {
        let mut key: HKEY = ptr::null_mut();
        let path = wide(LXSS);
        // SAFETY: `path` is NUL-terminated; `key` receives the handle.
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) }
            != ERROR_SUCCESS
        {
            return (Vec::new(), None);
        }
        let default = string_value(key, None, "DefaultDistribution");
        let mut distributions = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: `len` is the buffer's length in characters; the
            // optional outputs are null.
            let status = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if status != ERROR_SUCCESS {
                break;
            }
            let id = String::from_utf16_lossy(&name[..len as usize]);
            if let Some(distribution) = string_value(key, Some(&id), "DistributionName") {
                distributions.push((id, distribution));
            }
        }
        // SAFETY: `key` was opened above and isn't used afterwards.
        unsafe { RegCloseKey(key) };
        (distributions, default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etc_shells() {
        let text = "# /etc/shells: valid login shells\n/bin/sh\n/bin/bash\n\
                    /usr/bin/fish # fish\n\n/bin/bash\n";
        assert_eq!(
            parse_etc_shells(text),
            ["/bin/sh", "/bin/bash", "/usr/bin/fish"]
        );
    }

    #[test]
    fn default_distribution_first_without_docker() {
        let distributions = vec![
            ("{a}".into(), "docker-desktop".into()),
            ("{b}".into(), "Debian".into()),
            ("{c}".into(), "Ubuntu".into()),
            ("{d}".into(), "docker-desktop-data".into()),
        ];
        let found = order_distributions(distributions, Some("{C}"));
        assert_eq!(
            found,
            [
                Found::new("Ubuntu", "The default distribution."),
                Found::new("Debian", ""),
            ]
        );
    }
}
