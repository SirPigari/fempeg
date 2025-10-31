use anyhow::{Result, anyhow};
use serde_json::{Value as JsonValue, from_slice};
use std::collections::HashMap;
use std::{path::Path, process::Command};

#[cfg(target_os = "windows")]
use include_dir::{Dir, include_dir};
#[cfg(target_os = "windows")]
use std::{env, fs, io::Write};

#[cfg(not(target_os = "windows"))]
use crate::term_colors::{blue, green, pink, red, white};

#[cfg(target_os = "windows")]
static EXIFTOOL_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/exiftool_files");
#[cfg(target_os = "windows")]
static EXIFTOOL_EXECUTABLE: &[u8] = include_bytes!("../assets/exiftool.exe");
#[cfg(target_os = "windows")]
static EXIFTOOL_VERSION: &str = "13.40";

fn merge_json_output(output: &[u8]) -> Result<JsonValue> {
    let parsed: Vec<JsonValue> = from_slice(output)?;
    Ok(if parsed.len() == 1 {
        parsed.into_iter().next().unwrap()
    } else {
        JsonValue::Object(
            parsed
                .into_iter()
                .flat_map(|v| v.as_object().cloned())
                .flatten()
                .collect(),
        )
    })
}

#[cfg(not(target_os = "windows"))]
fn linux_macos_install_hint(e: &dyn std::fmt::Display) -> anyhow::Error {
    #[cfg(target_os = "linux")]
    {
        let head = blue("Failed to execute exiftool:");
        let hint = white(format!(
            " Please install it using your package manager:\n  sudo {} install exiftool   {}\n  sudo {} install perl-image-exiftool {}\n  sudo {} -S exiftool         {}",
            pink("apt"),
            green("# Ubuntu/Debian"),
            pink("dnf"),
            green("# Fedora"),
            pink("pacman"),
            green("# Arch Linux")
        ));
        anyhow!(format!("{}{} {}", head, white(format!(" {}", e)), hint))
    }

    #[cfg(target_os = "macos")]
    {
        let head = blue("Failed to execute exiftool:");
        let hint = white(" Please install it using Homebrew:\n  brew install exiftool");
        anyhow!(format!("{}{} {}", head, white(format!(" {}", e)), hint))
    }
}

#[cfg(target_os = "windows")]
pub fn call_exiftool(path: &Path) -> Result<JsonValue> {
    let pid = std::process::id();
    let temp_root = env::temp_dir().join(format!("exiftool_runtime_{}", pid));
    let exiftool_root = temp_root.join("exiftool_files");

    fs::create_dir_all(&exiftool_root)?;

    let exe_path = temp_root.join("exiftool.exe");
    fs::write(&exe_path, EXIFTOOL_EXECUTABLE)?;

    fn write_dir(base: &Path, dir: &Dir) -> Result<()> {
        for entry in dir.entries() {
            match entry {
                include_dir::DirEntry::Dir(subdir) => {
                    let subdir_path = base.join(subdir.path());
                    fs::create_dir_all(&subdir_path)?;
                    write_dir(base, subdir)?;
                }
                include_dir::DirEntry::File(file) => {
                    let dest_path = base.join(file.path());
                    if let Some(parent) = dest_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let mut f = fs::File::create(&dest_path)?;
                    f.write_all(file.contents())?;
                }
            }
        }
        Ok(())
    }

    write_dir(&exiftool_root, &EXIFTOOL_DIR)?;

    let canonicalized_path = path.canonicalize().unwrap_or(path.to_path_buf());
    let output = Command::new(&exe_path)
        .current_dir(&temp_root)
        .args(["-j", "-G1", "-a", "-n", "-json"])
        .arg(&canonicalized_path)
        .output()
        .map_err(|e| anyhow!("failed to execute exiftool: {}", e))?;

    let _ = fs::remove_dir_all(&temp_root);

    if !output.status.success() {
        return Err(anyhow!(
            "exiftool failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    merge_json_output(&output.stdout)
}

#[cfg(not(target_os = "windows"))]
pub fn call_exiftool(path: &Path) -> Result<JsonValue> {
    let canonicalized_path = path.canonicalize().unwrap_or(path.to_path_buf());
    match Command::new("exiftool")
        .args(["-j", "-G1", "-a", "-n", "-json"])
        .arg(&canonicalized_path)
        .output()
    {
        Ok(output) if output.status.success() => merge_json_output(&output.stdout),
        Ok(output) => Err(anyhow!(
            "exiftool failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(e) => Err(linux_macos_install_hint(&e)),
    }
}

#[cfg(target_os = "windows")]
pub fn get_exiftool_version() -> Result<String> {
    Ok(EXIFTOOL_VERSION.to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn get_exiftool_version() -> Result<String> {
    match Command::new("exiftool").arg("-ver").output() {
        Ok(out) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(anyhow!(
                "exiftool exited with non-zero status ({}): {}",
                red(out.status.code().unwrap_or(-1)),
                stderr
            ))
        }
        Err(e) => Err(linux_macos_install_hint(&e)),
    }
}

pub fn parse_exiftool_json(json: &JsonValue) -> Result<HashMap<String, String>> {
    let mut exif_data = HashMap::new();

    if let JsonValue::Object(map) = json {
        for (key, value) in map {
            exif_data.insert(
                key.clone(),
                value
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| value.to_string()),
            );
        }
    }

    Ok(exif_data)
}
