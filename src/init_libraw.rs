#[cfg(target_os = "windows")]
use anyhow::Context;
use anyhow::Result;
use libloading::Library;

use std::path::PathBuf;
use std::sync::OnceLock;
#[cfg(target_os = "windows")]
use std::{env, fs};

use crate::term_colors::{blue, white};
#[cfg(target_os = "linux")]
use crate::term_colors::{green, pink};

static LIB: OnceLock<Result<Library>> = OnceLock::new();

#[cfg(target_os = "windows")]
const LIBRAW_DLL: &[u8] = include_bytes!("../assets/libraw.dll");

pub fn init_libraw() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let tmp_dir = env::temp_dir().join("fempeg_libraw");
        fs::create_dir_all(&tmp_dir)
            .with_context(|| format!("Failed to create temp directory {:?}", tmp_dir))?;

        let dll_path = tmp_dir.join("libraw_c.dll");
        if !dll_path.exists() {
            fs::write(&dll_path, LIBRAW_DLL)
                .with_context(|| format!("Failed to write libraw DLL to {:?}", dll_path))?;
        }

        Ok(dll_path)
    }

    #[cfg(target_os = "linux")]
    {
        Ok(PathBuf::from("libraw.so"))
    }

    #[cfg(target_os = "macos")]
    {
        Ok(PathBuf::from("libraw.dylib"))
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let lib_path = std::env::var("LIBRAW_PATH").unwrap_or_else(|_| {
            eprintln!(
                "Unsupported OS for libraw, defaulting to libraw.so (env {} not set)",
                blue("LIBRAW_PATH")
            );
            "libraw.so".to_string()
        });

        Ok(PathBuf::from(lib_path))
    }
}

pub fn get_lib() -> Result<&'static Library> {
    LIB.get_or_init(|| {
        let lib_path = init_libraw().unwrap();

        unsafe { Library::new(&lib_path) }.map_err(|e| {
            #[cfg(target_os = "windows")]
            {
                let head = blue("Failed to load internal libraw DLL:");
                let body = white(format!(" {}", e));
                anyhow::anyhow!(format!("{}{}", head, body))
            }

            #[cfg(target_os = "linux")]
            {
                let head = blue("Failed to load system libraw:");
                let hint = white(format!(
                    " Please install it using your package manager:\n  sudo {} install libraw-dev   {}\n  sudo {} install libraw       {}\n  sudo {} -S libraw         {} ",
                    pink("apt"), green("# Ubuntu/Debian"), pink("dnf"), green("# Fedora"), pink("pacman"), green("# Arch Linux")
                ));
                anyhow::anyhow!(format!("{}{} {}", head, white(format!(" {}", e)), hint))
            }

            #[cfg(target_os = "macos")]
            {
                let head = blue("Failed to load system libraw:");
                let hint = white(" Please install it using Homebrew:\n  brew install libraw");
                anyhow::anyhow!(format!("{}{} {}", head, white(format!(" {}", e)), hint))
            }
        })
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!(e))
}
