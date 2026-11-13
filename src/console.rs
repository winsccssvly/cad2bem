// SPDX-License-Identifier: GPL-3.0-or-later
//! Keep normal console CLI semantics. On Windows 11 24H2 and later the manifest gives the GUI no console when it
//! is opened from Explorer. On older Windows the GUI that finds a console of its own (Explorer made it) starts
//! itself again without one and exits, so that console closes at once: a window that only flashes, and no
//! terminal tab left behind. Without a console, a GUI that cannot start says why in a message box.

use std::ffi::OsString;

/// Set for a GUI started again without a console: its errors go to a message box.
const DETACHED: &str = "CAD2BEM_DETACHED_GUI";

/// Whether this process is alone on its console: Windows made the console for it (opened from Explorer before
/// Windows 11 24H2). A terminal's console is shared with the shell.
pub fn owns_alone() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleProcessList(processes: *mut u32, capacity: u32) -> u32;
        }
        let mut attached = [0_u32; 2];
        // SAFETY: valid two-element DWORD buffer; the count is returned, zero without a console.
        let count = unsafe { GetConsoleProcessList(attached.as_mut_ptr(), 2) };
        count == 1 && attached[0] == std::process::id()
    }
    #[cfg(not(windows))]
    false
}

/// Start this executable again with `arguments` and no console (the GUI), and return once it has started.
pub fn relaunch_detached(arguments: &[OsString]) -> std::io::Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(arguments)
        .env(DETACHED, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(DETACHED_PROCESS);
    }
    command.spawn().map(drop)
}

/// The fallback when the GUI could not start again without a console: release the console it owns once the
/// window is up.
pub fn gui_ready() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleProcessList(processes: *mut u32, capacity: u32) -> u32;
            fn FreeConsole() -> i32;
        }
        let mut attached = [0_u32; 1];
        // SAFETY: valid one-element DWORD buffer. A count above capacity means a shared console;
        // zero means no console/error. Neither case is changed. FreeConsole affects this process only.
        unsafe {
            if GetConsoleProcessList(attached.as_mut_ptr(), 1) == 1
                && attached[0] == std::process::id()
            {
                let _ = FreeConsole();
            }
        }
    }
}

/// Whether the process has a console window (a terminal's, or one Windows made for it).
pub fn window() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleWindow() -> isize;
        }
        // SAFETY: no arguments; returns a window handle or zero.
        unsafe { GetConsoleWindow() != 0 }
    }
    #[cfg(not(windows))]
    true
}

/// Whether nobody would read the error output: no console and no redirected standard error (opened from
/// Explorer), or a GUI started again without a console. A program that runs this one with its output captured gets the error there, never a message box.
pub fn unseen() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetStdHandle(which: u32) -> isize;
        }
        const STD_ERROR_HANDLE: u32 = -12_i32 as u32;
        // SAFETY: neither call takes pointers; each returns a handle, zero or INVALID_HANDLE_VALUE (-1).
        unsafe {
            let error = GetStdHandle(STD_ERROR_HANDLE);
            !window() && (error == 0 || error == -1 || std::env::var_os(DETACHED).is_some())
        }
    }
    #[cfg(not(windows))]
    false
}

/// Show an error to a user who has no console.
pub fn message_box(message: &str) {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(window: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
        }
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (text, caption) = (wide(message), wide("cad2bem"));
        const MB_ICONERROR: u32 = 0x10;
        // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call; no owner window.
        unsafe {
            MessageBoxW(0, text.as_ptr(), caption.as_ptr(), MB_ICONERROR);
        }
    }
    #[cfg(not(windows))]
    let _ = message;
}
