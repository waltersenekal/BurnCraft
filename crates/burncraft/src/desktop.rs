//! Linux desktop integration: the menu entry and the icon that panels and task bars show.
//!
//! Wayland has no protocol for a window to set its task-bar icon. The shell instead looks up the
//! window's `app_id` among the installed `.desktop` files and uses that entry's `Icon=`. A bare
//! binary started from a terminal therefore keeps the desktop's placeholder icon until this entry
//! and the themed icon are installed. Both go in the user's XDG data directory, so no root access
//! is needed and nothing outside the home folder is touched.

use std::path::PathBuf;

/// The window's app id, which the `.desktop` file is named after and which is also its
/// `StartupWMClass`, so the shell can tell that a running window belongs to that entry.
pub const APP_ID: &str = "org.burncraft.BurnCraft";

/// Handles `--install-desktop` and `--uninstall-desktop`, returning the exit code when one of them
/// was given; `None` means this is an ordinary launch.
///
/// This runs before the log, the crash handler and the single-instance lock: installing the icon
/// has to work while BurnCraft is already open, and a graphics driver failing to start the window
/// has nothing to do with a menu entry.
pub fn command_line() -> Option<i32> {
    let mut wants_install = false;
    let mut wants_uninstall = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--install-desktop" => wants_install = true,
            "--uninstall-desktop" => wants_uninstall = true,
            _ => {}
        }
    }
    if !wants_install && !wants_uninstall {
        return None;
    }
    let result = match (wants_install, wants_uninstall) {
        (true, true) => Err("Use either --install-desktop or --uninstall-desktop, not both.".to_string()),
        (false, true) => uninstall().map(|entry| format!("Removed {} and the BurnCraft icon.", entry.display())),
        _ => install().map(|entry| {
            format!(
                "Installed the BurnCraft menu entry at {}\n\
                 The next launch shows the BurnCraft icon in the panel and task bar.",
                entry.display()
            )
        }),
    };
    Some(match result {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(text) => {
            eprintln!("{text}");
            // A release build on Windows has no console, so that message would otherwise be lost.
            #[cfg(windows)]
            {
                crate::startup::message_box("BurnCraft", &text, false, false);
            }
            1
        }
    })
}

/// A PNG that build.rs drew, so the program carries its icon instead of shipping image files.
#[cfg(target_os = "linux")]
macro_rules! icon_png {
    ($size:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/icon-", stringify!($size), ".png"))
    };
}

/// The icon sizes the build draws and this module installs. Panels and menus ask for different
/// sizes and the shell scales the nearest one, so installing the usual range keeps them all crisp.
#[cfg(target_os = "linux")]
const ICONS: [(u32, &[u8]); 7] = [
    (16, icon_png!(16)),
    (24, icon_png!(24)),
    (32, icon_png!(32)),
    (48, icon_png!(48)),
    (64, icon_png!(64)),
    (128, icon_png!(128)),
    (256, icon_png!(256)),
];

/// Writes the menu entry and the icon for the current user, returning the entry it wrote.
#[cfg(target_os = "linux")]
pub fn install() -> Result<PathBuf, String> {
    let data = data_home()?;
    let apps = data.join("applications");
    std::fs::create_dir_all(&apps).map_err(|e| format!("Could not create {}: {e}", apps.display()))?;

    // The entry starts the exact binary that is running now, so it keeps working when BurnCraft
    // was run from a build directory. Moving or rebuilding the binary needs another install.
    let exe = std::env::current_exe().map_err(|e| format!("Could not find the BurnCraft executable: {e}"))?;
    let entry = apps.join(format!("{APP_ID}.desktop"));
    std::fs::write(&entry, entry_text(&exec_path(&exe))).map_err(|e| format!("Could not write {}: {e}", entry.display()))?;

    let icons = data.join("icons").join("hicolor");
    for (size, png) in ICONS {
        let dir = icons.join(format!("{size}x{size}")).join("apps");
        std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        let path = dir.join(format!("{APP_ID}.png"));
        std::fs::write(&path, png).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    }

    refresh_caches(&data);
    Ok(entry)
}

/// Removes the menu entry and every icon size, returning the entry that was removed.
#[cfg(target_os = "linux")]
pub fn uninstall() -> Result<PathBuf, String> {
    let data = data_home()?;
    let entry = data.join("applications").join(format!("{APP_ID}.desktop"));
    remove(&entry)?;
    // Every size directory rather than the seven above, so sizes an older BurnCraft installed are
    // removed too. Anything else in those folders belongs to other programs and stays.
    if let Ok(themes) = std::fs::read_dir(data.join("icons").join("hicolor")) {
        for theme in themes.flatten() {
            // That folder also holds the icon-theme.cache file, not just the "128x128" folders.
            if !theme.path().is_dir() {
                continue;
            }
            remove(&theme.path().join("apps").join(format!("{APP_ID}.png")))?;
        }
    }
    refresh_caches(&data);
    Ok(entry)
}

#[cfg(not(target_os = "linux"))]
pub fn install() -> Result<PathBuf, String> {
    Err("Nothing to install here: Windows shows the icon embedded in burncraft.exe, and the macOS \
         .app carries its own. The menu entry is only needed on Linux."
        .into())
}

#[cfg(not(target_os = "linux"))]
pub fn uninstall() -> Result<PathBuf, String> {
    Err("The menu entry is only installed on Linux, so there is nothing to remove here.".into())
}

/// The text of the `.desktop` file. `exec` already carries its quoting and field code.
#[cfg(target_os = "linux")]
fn entry_text(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=BurnCraft\n\
         GenericName=Laser cutter software\n\
         Comment=Design, prepare and run jobs on laser cutters and engravers\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Graphics;Engineering;\n\
         Keywords=laser;lasercut;engrave;cut;svg;dxf;\n\
         StartupNotify=true\n\
         StartupWMClass={APP_ID}\n"
    )
}

/// Quotes a path for the Desktop Entry `Exec` key and passes a file dropped on the icon through:
/// BurnCraft opens a path given as its first argument, and `%f` is the field code for one file.
#[cfg(target_os = "linux")]
fn exec_path(path: &std::path::Path) -> String {
    // A backslash and a quote would end the quoted path early; `%` starts a field code, so it
    // doubles.
    let text = path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    format!("\"{text}\" %f")
}

/// Where per-user data lives: `$XDG_DATA_HOME`, or `~/.local/share` when that is unset or not an
/// absolute path, as the XDG base directory spec says.
#[cfg(target_os = "linux")]
fn data_home() -> Result<PathBuf, String> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .ok_or_else(|| "Neither XDG_DATA_HOME nor HOME is set, so there is nowhere to install to.".to_string())
}

/// Deletes a file, treating "it was not there" as success: installing twice or removing twice is
/// not an error.
#[cfg(target_os = "linux")]
fn remove(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not remove {}: {e}", path.display())),
    }
}

/// Refreshes the caches the desktop and the icon theme keep for the entry. Both tools are
/// optional, their diagnostics are of no use to someone who just wants the icon, and a missing one
/// only means the desktop notices the change by itself, so failures are ignored on purpose.
///
/// KDE's `kbuildsycoca` is deliberately not called: Plasma rebuilds that cache when the
/// applications folder changes, and the tool has its own configuration that it reports through a
/// warning dialog - not something installing an icon should ever put on someone's screen.
#[cfg(target_os = "linux")]
fn refresh_caches(data: &std::path::Path) {
    use std::ffi::OsStr;
    let apps = data.join("applications");
    let icons = data.join("icons").join("hicolor");
    run("update-desktop-database", &[apps.as_os_str()]);
    run("gtk-update-icon-cache", &[OsStr::new("-f"), OsStr::new("-t"), icons.as_os_str()]);
}

/// How long one cache tool may take before it is given up on. They normally finish in a second or
/// two, but on a network home folder or a busy session they can wait for a long time, and a command
/// that installs an icon must not look hung.
#[cfg(target_os = "linux")]
const CACHE_TOOL_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(target_os = "linux")]
fn run(command: &str, args: &[&std::ffi::OsStr]) {
    let Ok(mut child) = std::process::Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = std::time::Instant::now() + CACHE_TOOL_PATIENCE;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn exec_quotes_spaces_and_field_codes() {
        assert_eq!(exec_path(std::path::Path::new("/home/me/Burn Craft/burncraft")), "\"/home/me/Burn Craft/burncraft\" %f");
        assert_eq!(exec_path(std::path::Path::new("/tmp/100%/burncraft")), "\"/tmp/100%%/burncraft\" %f");
        assert_eq!(exec_path(std::path::Path::new("/tmp/a\"b/c\\d/burncraft")), "\"/tmp/a\\\"b/c\\\\d/burncraft\" %f");
    }

    #[test]
    fn entry_agrees_with_the_window() {
        let entry = entry_text("\"/opt/BurnCraft/burncraft\" %f");
        for line in [
            "Type=Application",
            "Name=BurnCraft",
            "Exec=\"/opt/BurnCraft/burncraft\" %f",
            "Icon=org.burncraft.BurnCraft",
            "StartupWMClass=org.burncraft.BurnCraft",
            "Terminal=false",
        ] {
            assert!(entry.contains(line), "the desktop entry has no `{line}`:\n{entry}");
        }
    }

    #[test]
    fn every_size_is_a_png() {
        assert_eq!(ICONS.len(), 7);
        for (size, png) in ICONS {
            assert!((16..=256).contains(&size), "unexpected icon size {size}");
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "the {size} px icon is not a PNG");
        }
    }
}
