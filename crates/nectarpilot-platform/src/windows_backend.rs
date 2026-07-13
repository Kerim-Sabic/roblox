//! Narrow Win32 adapters for the portable safety abstractions.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::size_of;
use std::path::PathBuf;
use std::rc::Rc;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, RegisterHotKey, SendInput, UnregisterHotKey, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetClientRect, GetForegroundWindow, GetSystemMetrics, GetWindow,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, MSG, PM_REMOVE,
    PeekMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    WM_HOTKEY,
};
use windows::core::{BOOL, HRESULT, PWSTR};

use crate::emergency::{EmergencyHotkeyBackend, EmergencyStopError};
use crate::input::{BrokerError, InputAction, InputSink, Key, MouseButton};
use crate::process::{ProcessController, ProcessError, ProcessIdentity};
use crate::session::{
    ProcessId, Rect, SessionError, SessionProbe, SessionTarget, WindowGeometry, WindowHandle,
    WindowSnapshot,
};

pub const EMERGENCY_HOTKEY_ID: i32 = 0x4E50;

/// A read-only candidate discovered by enumerating Windows process metadata and
/// attaching only to a matching top-level `RobloxPlayerBeta.exe` window. It is
/// not an adoption: callers must still explicitly choose an exact PID before
/// any automation backend can be constructed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveredRobloxClient {
    pub pid: ProcessId,
    pub window: Option<WindowSnapshot>,
}

/// Lists official Roblox player processes without injecting input, reading
/// memory, or modifying any process. A process without an attachable window is
/// returned as a loading/hidden candidate rather than silently ignored.
pub fn discover_roblox_clients() -> Result<Vec<DiscoveredRobloxClient>, SessionError> {
    // SAFETY: This creates a read-only process snapshot for the current
    // machine. The returned handle is closed on every path below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|error| SessionError::Platform(error.to_string()))?;
    let result = enumerate_roblox_processes(snapshot);
    // SAFETY: `snapshot` is an owned handle returned by
    // CreateToolhelp32Snapshot and has not been transferred elsewhere.
    let _ = unsafe { CloseHandle(snapshot) };
    result
}

fn enumerate_roblox_processes(
    snapshot: HANDLE,
) -> Result<Vec<DiscoveredRobloxClient>, SessionError> {
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..PROCESSENTRY32W::default()
    };
    // SAFETY: `entry` is an initialized writable PROCESSENTRY32W and snapshot
    // is a valid Tool Help snapshot handle owned by the caller.
    unsafe { Process32FirstW(snapshot, &raw mut entry) }
        .map_err(|error| SessionError::Platform(error.to_string()))?;

    let probe = WindowsSessionProbe;
    let mut clients = Vec::new();
    loop {
        if process_entry_name(&entry).eq_ignore_ascii_case("RobloxPlayerBeta.exe")
            && let Some(pid) = ProcessId::new(entry.th32ProcessID)
        {
            clients.push(DiscoveredRobloxClient {
                pid,
                window: probe.find_main_window(pid).ok(),
            });
        }
        entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
            ..PROCESSENTRY32W::default()
        };
        // SAFETY: as above; ERROR_NO_MORE_FILES is the expected end condition.
        if unsafe { Process32NextW(snapshot, &raw mut entry) }.is_err() {
            break;
        }
    }
    Ok(clients)
}

fn process_entry_name(entry: &PROCESSENTRY32W) -> String {
    let length = entry
        .szExeFile
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..length])
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsSessionProbe;

impl SessionProbe for WindowsSessionProbe {
    fn find_main_window(&self, pid: ProcessId) -> Result<WindowSnapshot, SessionError> {
        let mut search = WindowSearch {
            pid: pid.get(),
            found: None,
        };
        // SAFETY: `search` remains alive and exclusively borrowed for the duration
        // of the synchronous EnumWindows call. The callback never retains LPARAM.
        unsafe {
            EnumWindows(
                Some(enum_window_for_pid),
                LPARAM((&raw mut search).cast::<c_void>() as isize),
            )
        }
        .map_err(|error| SessionError::Platform(error.to_string()))?;
        let window = search
            .found
            .ok_or(SessionError::WindowNotFound(pid.get()))?;
        self.snapshot(SessionTarget { pid, window })
    }

    fn snapshot(&self, target: SessionTarget) -> Result<WindowSnapshot, SessionError> {
        let hwnd = to_hwnd(target.window);
        // SAFETY: HWND is revalidated with IsWindow before all subsequent queries.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err(SessionError::WindowNotFound(target.pid.get()));
        }
        let actual = window_process_id(hwnd)?;
        if actual != target.pid {
            return Err(SessionError::OwnershipChanged);
        }
        let geometry = window_geometry(hwnd)?;
        // SAFETY: GetForegroundWindow has no preconditions.
        let foreground = unsafe { GetForegroundWindow() };
        Ok(WindowSnapshot {
            target,
            geometry,
            is_foreground: foreground == hwnd,
        })
    }

    fn foreground_target(&self) -> Result<Option<SessionTarget>, SessionError> {
        Ok(foreground_target())
    }
}

struct WindowSearch {
    pid: u32,
    found: Option<WindowHandle>,
}

unsafe extern "system" fn enum_window_for_pid(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumWindows invokes this callback synchronously with the valid
    // pointer supplied by find_main_window.
    let search = unsafe { &mut *(data.0 as *mut WindowSearch) };
    if search.found.is_some() {
        return BOOL(1);
    }
    let mut candidate_pid = 0_u32;
    // SAFETY: The callback receives a system-provided HWND and candidate_pid is
    // a valid writable out parameter.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut candidate_pid)) };
    // SAFETY: Visibility and owner queries are read-only for the supplied HWND.
    let usable = candidate_pid == search.pid
        && unsafe { IsWindowVisible(hwnd) }.as_bool()
        && unsafe { GetWindow(hwnd, GW_OWNER) }.is_err();
    if usable {
        search.found = from_hwnd(hwnd);
    }
    BOOL(1)
}

fn window_process_id(hwnd: HWND) -> Result<ProcessId, SessionError> {
    let mut pid = 0_u32;
    // SAFETY: pid is a valid writable out parameter; HWND was supplied by the OS
    // or revalidated by the caller.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    ProcessId::new(pid).ok_or(SessionError::OwnershipChanged)
}

fn window_geometry(hwnd: HWND) -> Result<WindowGeometry, SessionError> {
    let mut outer = RECT::default();
    let mut client = RECT::default();
    // SAFETY: both RECT values are initialized writable out parameters and HWND
    // has been checked by the caller.
    unsafe { GetWindowRect(hwnd, &raw mut outer) }
        .map_err(|error| SessionError::Platform(error.to_string()))?;
    // SAFETY: same as above for the client rectangle.
    unsafe { GetClientRect(hwnd, &raw mut client) }
        .map_err(|error| SessionError::Platform(error.to_string()))?;
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: origin is a valid writable point and HWND is valid.
    if !unsafe { ClientToScreen(hwnd, &raw mut origin) }.as_bool() {
        return Err(SessionError::Platform(
            windows::core::Error::from_win32().to_string(),
        ));
    }
    // SAFETY: monitor selection and information retrieval use a valid HWND and
    // a correctly sized MONITORINFO structure.
    let monitor_handle = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut monitor_info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(u32::MAX),
        ..MONITORINFO::default()
    };
    // SAFETY: monitor_info is a valid writable structure of the declared size.
    if !unsafe { GetMonitorInfoW(monitor_handle, &raw mut monitor_info) }.as_bool() {
        return Err(SessionError::Platform(
            windows::core::Error::from_win32().to_string(),
        ));
    }

    let outer = rect_from_native(outer)?;
    let client_width = positive_size(client.right - client.left)?;
    let client_height = positive_size(client.bottom - client.top)?;
    let monitor = rect_from_native(monitor_info.rcMonitor)?;
    let fullscreen = !unsafe { IsIconic(hwnd) }.as_bool()
        && outer.left <= monitor.left
        && outer.top <= monitor.top
        && outer.width >= monitor.width
        && outer.height >= monitor.height;
    // SAFETY: GetDpiForWindow is a read-only query for the validated HWND.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    Ok(WindowGeometry {
        outer,
        client: Rect {
            left: origin.x,
            top: origin.y,
            width: client_width,
            height: client_height,
        },
        monitor,
        dpi: if dpi == 0 { 96 } else { dpi },
        // SAFETY: IsIconic is a read-only query for the validated HWND.
        minimized: unsafe { IsIconic(hwnd) }.as_bool(),
        fullscreen,
    })
}

fn rect_from_native(rect: RECT) -> Result<Rect, SessionError> {
    Ok(Rect {
        left: rect.left,
        top: rect.top,
        width: positive_size(rect.right - rect.left)?,
        height: positive_size(rect.bottom - rect.top)?,
    })
}

fn positive_size(value: i32) -> Result<u32, SessionError> {
    u32::try_from(value)
        .ok()
        .filter(|size| *size > 0)
        .ok_or_else(|| SessionError::Platform("window reported an empty rectangle".to_owned()))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsInputSink;

impl InputSink for WindowsInputSink {
    fn foreground_target(&self) -> Result<Option<SessionTarget>, BrokerError> {
        Ok(foreground_target())
    }

    fn send(&mut self, target: SessionTarget, action: InputAction) -> Result<(), BrokerError> {
        let is_release = matches!(
            action,
            InputAction::KeyUp { .. } | InputAction::MouseUp { .. }
        );
        // Key/button releases are deliberately allowed after focus is lost: they
        // only unwind state injected by the broker and prevent a stuck movement
        // key. Every other event still requires the exact foreground PID/HWND.
        if !is_release && foreground_target() != Some(target) {
            return Err(BrokerError::WrongForeground);
        }
        let input = match action {
            InputAction::KeyDown { key } => keyboard_input(key, false)?,
            InputAction::KeyUp { key } => keyboard_input(key, true)?,
            InputAction::MouseDown { button } => mouse_button_input(button, false),
            InputAction::MouseUp { button } => mouse_button_input(button, true),
            InputAction::MouseMoveClient { x, y } => mouse_move_input(target.window, x, y)?,
            InputAction::MouseWheel { delta } => mouse_wheel_input(delta),
        };
        // SAFETY: INPUT is fully initialized for its selected union variant and
        // SendInput receives the exact native structure size.
        let sent = unsafe {
            SendInput(
                &[input],
                i32::try_from(size_of::<INPUT>()).expect("INPUT fits in i32"),
            )
        };
        if sent == 1 {
            Ok(())
        } else {
            Err(BrokerError::Backend(
                "SendInput did not accept the event".to_owned(),
            ))
        }
    }
}

/// Sends one global key press/release pair for `virtual_key`. Used only to
/// toggle the generated walk harness's F16 pause handler — an `AutoHotkey`
/// hotkey fires regardless of which window has focus, so this deliberately
/// bypasses the focus-guarded input broker.
pub fn tap_global_virtual_key(virtual_key: u16) -> Result<(), BrokerError> {
    for released in [false, true] {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(virtual_key),
                    dwFlags: if released {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS::default()
                    },
                    ..KEYBDINPUT::default()
                },
            },
        };
        // SAFETY: INPUT is fully initialized for the keyboard variant and
        // SendInput receives the exact native structure size.
        let sent = unsafe {
            SendInput(
                &[input],
                i32::try_from(size_of::<INPUT>()).expect("INPUT fits in i32"),
            )
        };
        if sent != 1 {
            return Err(BrokerError::Backend(
                "SendInput did not accept the global key event".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Thread-affine registration of several user-configured control chords.
/// Construct and poll on one dedicated thread; identifiers are the caller's.
pub struct WindowsHotkeySet {
    ids: Vec<i32>,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl WindowsHotkeySet {
    /// Registers `(id, modifiers, virtual_key)` chords. Registration failures
    /// (for example a chord already owned by another app) unwind the set.
    pub fn register(bindings: &[(i32, u32, u32)]) -> Result<Self, EmergencyStopError> {
        let mut registered = Vec::with_capacity(bindings.len());
        for (id, modifiers, virtual_key) in bindings {
            if !(0..=0xBFFF).contains(id) {
                unregister_ids(&registered);
                return Err(EmergencyStopError::Backend(
                    "global hotkey identifier must be in 0..=0xBFFF".to_owned(),
                ));
            }
            // SAFETY: a null HWND creates thread-associated hotkeys; this
            // object is !Send and unregisters on the same thread when dropped.
            let result = unsafe {
                RegisterHotKey(
                    None,
                    *id,
                    HOT_KEY_MODIFIERS(*modifiers) | MOD_NOREPEAT,
                    *virtual_key,
                )
            };
            if let Err(error) = result {
                unregister_ids(&registered);
                return Err(EmergencyStopError::Backend(format!(
                    "could not register global hotkey {id}: {error}"
                )));
            }
            registered.push(*id);
        }
        Ok(Self {
            ids: registered,
            _thread_affinity: PhantomData,
        })
    }

    /// Drains pending hotkey messages, returning the pressed identifiers.
    #[must_use]
    pub fn poll_pressed(&mut self) -> Vec<i32> {
        let mut pressed = Vec::new();
        let mut message = MSG::default();
        // SAFETY: message is a valid writable structure. Filtering to WM_HOTKEY
        // consumes only registered hotkey messages for this thread.
        while unsafe { PeekMessageW(&raw mut message, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE) }
            .as_bool()
        {
            if let Ok(id) = i32::try_from(message.wParam.0)
                && self.ids.contains(&id)
            {
                pressed.push(id);
            }
        }
        pressed
    }
}

impl Drop for WindowsHotkeySet {
    fn drop(&mut self) {
        unregister_ids(&self.ids);
    }
}

fn unregister_ids(ids: &[i32]) {
    for id in ids {
        // SAFETY: unregisters only identifiers this thread registered.
        let _ = unsafe { UnregisterHotKey(None, *id) };
    }
}

/// Thread-affine registration for the hard Ctrl+Shift+F12 stop chord.
/// Construct and poll this value on the automation supervisor thread.
pub struct WindowsEmergencyHotkey {
    id: i32,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl WindowsEmergencyHotkey {
    pub fn register_default() -> Result<Self, EmergencyStopError> {
        Self::register(EMERGENCY_HOTKEY_ID)
    }

    pub fn register(id: i32) -> Result<Self, EmergencyStopError> {
        if !(0..=0xBFFF).contains(&id) {
            return Err(EmergencyStopError::Backend(
                "global hotkey identifier must be in 0..=0xBFFF".to_owned(),
            ));
        }
        // SAFETY: a null HWND creates a thread-associated hotkey. This object is
        // !Send and unregisters on the same thread when dropped.
        unsafe {
            RegisterHotKey(
                None,
                id,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                u32::from(0x7B_u16),
            )
        }
        .map_err(|error| EmergencyStopError::Backend(error.to_string()))?;
        Ok(Self {
            id,
            _thread_affinity: PhantomData,
        })
    }
}

impl EmergencyHotkeyBackend for WindowsEmergencyHotkey {
    fn poll_triggered(&mut self) -> Result<bool, EmergencyStopError> {
        let mut message = MSG::default();
        // SAFETY: message is a valid writable structure. Filtering to WM_HOTKEY
        // consumes only registered hotkey messages for this thread.
        while unsafe { PeekMessageW(&raw mut message, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE) }
            .as_bool()
        {
            if message.wParam.0 == usize::try_from(self.id).unwrap_or(usize::MAX) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Drop for WindowsEmergencyHotkey {
    fn drop(&mut self) {
        // SAFETY: this object unregisters the same thread-associated identifier
        // it registered and its !Send marker prevents cross-thread movement.
        let _ = unsafe { UnregisterHotKey(None, self.id) };
    }
}

fn keyboard_input(key: Key, released: bool) -> Result<INPUT, BrokerError> {
    let virtual_key = match key {
        Key::Forward => 0x57,
        Key::Backward => 0x53,
        Key::Left => 0x41,
        Key::Right => 0x44,
        Key::Jump => 0x20,
        Key::Escape => 0x1B,
        Key::Interact => 0x45,
        Key::Shift => 0x10,
        Key::Control => 0x11,
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F12 => 0x7B,
        Key::Digit(value) if value <= 9 => u16::from(b'0') + u16::from(value),
        Key::Digit(_) => return Err(BrokerError::InvalidKey),
    };
    Ok(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(virtual_key),
                dwFlags: if released {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS::default()
                },
                ..KEYBDINPUT::default()
            },
        },
    })
}

fn mouse_button_input(button: MouseButton, released: bool) -> INPUT {
    let flags = match (button, released) {
        (MouseButton::Left, false) => MOUSEEVENTF_LEFTDOWN,
        (MouseButton::Left, true) => MOUSEEVENTF_LEFTUP,
        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTDOWN,
        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTUP,
        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEUP,
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: flags,
                ..MOUSEINPUT::default()
            },
        },
    }
}

fn mouse_wheel_input(delta: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                mouseData: u32::from_ne_bytes(delta.to_ne_bytes()),
                dwFlags: MOUSEEVENTF_WHEEL,
                ..MOUSEINPUT::default()
            },
        },
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "validated normalized coordinates map to Windows' bounded 0..65535 range"
)]
fn mouse_move_input(
    window: WindowHandle,
    normalized_x: f32,
    normalized_y: f32,
) -> Result<INPUT, BrokerError> {
    let hwnd = to_hwnd(window);
    let geometry =
        window_geometry(hwnd).map_err(|error| BrokerError::Backend(error.to_string()))?;
    // SAFETY: GetSystemMetrics has no preconditions for these virtual-screen indices.
    let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    // SAFETY: same as above.
    let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    // SAFETY: same as above.
    let virtual_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    // SAFETY: same as above.
    let virtual_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if virtual_width <= 1 || virtual_height <= 1 {
        return Err(BrokerError::Backend(
            "Windows reported an invalid virtual desktop".to_owned(),
        ));
    }
    let client_x = f64::from(geometry.client.left)
        + f64::from(normalized_x) * f64::from(geometry.client.width.saturating_sub(1));
    let client_y = f64::from(geometry.client.top)
        + f64::from(normalized_y) * f64::from(geometry.client.height.saturating_sub(1));
    let absolute_x = ((client_x - f64::from(virtual_left)) * 65_535.0
        / f64::from(virtual_width - 1))
    .round() as i32;
    let absolute_y = ((client_y - f64::from(virtual_top)) * 65_535.0
        / f64::from(virtual_height - 1))
    .round() as i32;
    Ok(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: absolute_x,
                dy: absolute_y,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ..MOUSEINPUT::default()
            },
        },
    })
}

fn foreground_target() -> Option<SessionTarget> {
    // SAFETY: GetForegroundWindow has no preconditions.
    let hwnd = unsafe { GetForegroundWindow() };
    let window = from_hwnd(hwnd)?;
    let mut pid = 0_u32;
    // SAFETY: pid is a valid writable out parameter and HWND came from Windows.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    ProcessId::new(pid).map(|pid| SessionTarget { pid, window })
}

fn from_hwnd(hwnd: HWND) -> Option<WindowHandle> {
    WindowHandle::new(hwnd.0 as usize as u64)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "NectarPilot supports 64-bit Windows, where HWND and u64 have equal width"
)]
fn to_hwnd(window: WindowHandle) -> HWND {
    HWND(window.get() as usize as *mut c_void)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsProcessController;

impl ProcessController for WindowsProcessController {
    fn identity(&self, pid: ProcessId) -> Result<Option<ProcessIdentity>, ProcessError> {
        let handle = match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
            Ok(handle) => handle,
            Err(error) if error.code() == HRESULT::from_win32(87) => return Ok(None),
            Err(error) => return Err(ProcessError::Platform(error.to_string())),
        };
        process_identity(&handle, pid).map(Some)
    }

    fn terminate_exact(&mut self, identity: &ProcessIdentity) -> Result<(), ProcessError> {
        let handle = open_process(
            identity.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
        )
        .map_err(|error| ProcessError::Platform(error.to_string()))?;
        let current = process_identity(&handle, identity.pid)?;
        if &current != identity {
            return Err(ProcessError::IdentityChanged(identity.pid.get()));
        }
        // SAFETY: The handle was opened with PROCESS_TERMINATE and its identity
        // was revalidated immediately before this exact termination.
        unsafe { TerminateProcess(handle.0, 1) }
            .map_err(|error| ProcessError::Platform(error.to_string()))
    }
}

struct OwnedProcessHandle(HANDLE);

impl Drop for OwnedProcessHandle {
    fn drop(&mut self) {
        // SAFETY: this RAII wrapper owns the handle exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn open_process(
    pid: ProcessId,
    access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS,
) -> windows::core::Result<OwnedProcessHandle> {
    // SAFETY: PID is non-zero and the returned owned handle is closed by RAII.
    unsafe { OpenProcess(access, false, pid.get()) }.map(OwnedProcessHandle)
}

fn process_identity(
    handle: &OwnedProcessHandle,
    pid: ProcessId,
) -> Result<ProcessIdentity, ProcessError> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all FILETIME values are valid writable out parameters and the
    // process handle has query rights.
    unsafe {
        GetProcessTimes(
            handle.0,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .map_err(|error| ProcessError::Platform(error.to_string()))?;
    let created_at_ticks =
        (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    let executable_path = query_image_path(handle).ok();
    Ok(ProcessIdentity {
        pid,
        created_at_ticks,
        executable_path,
    })
}

fn query_image_path(handle: &OwnedProcessHandle) -> windows::core::Result<PathBuf> {
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: buffer is writable for `length` UTF-16 code units and the process
    // handle has query rights.
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_FORMAT::default(),
            PWSTR(buffer.as_mut_ptr()),
            &raw mut length,
        )
    }?;
    buffer.truncate(usize::try_from(length).unwrap_or(buffer.len()));
    Ok(PathBuf::from(String::from_utf16_lossy(&buffer)))
}

#[cfg(test)]
mod tests {
    use super::{PROCESSENTRY32W, process_entry_name};

    #[test]
    fn process_snapshot_name_stops_at_the_first_nul() {
        let mut entry = PROCESSENTRY32W::default();
        let encoded = "RobloxPlayerBeta.exe\0ignored"
            .encode_utf16()
            .collect::<Vec<_>>();
        entry.szExeFile[..encoded.len()].copy_from_slice(&encoded);
        assert_eq!(process_entry_name(&entry), "RobloxPlayerBeta.exe");
    }
}
