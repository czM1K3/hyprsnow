use hyprland::event_listener::AsyncEventListener;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::process::Command;
use std::sync::mpsc;
use std::thread;

#[derive(Clone)]
pub struct WindowRect {
    pub address: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

#[derive(Clone, Debug)]
pub struct MonitorRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub has_fullscreen: bool,
}

#[derive(Debug, Clone)]
pub enum HyprlandEvent {
    WindowsChanged,
}

// Minimal, lenient views of `hyprctl -j` output. The `hyprland` crate's data
// types break whenever Hyprland changes its JSON (e.g. special workspaces no
// longer carry an `id`), so we only deserialize the fields we need and make
// anything that may be absent optional.

#[derive(Deserialize, Default)]
struct WorkspaceRef {
    id: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprMonitor {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    #[serde(default = "default_scale")]
    scale: f32,
    #[serde(default)]
    transform: u8,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    active_workspace: WorkspaceRef,
}

fn default_scale() -> f32 {
    1.0
}

impl HyprMonitor {
    /// Size in layout (logical) coordinates, accounting for scale and rotation.
    fn logical_size(&self) -> (f32, f32) {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (w, h) = (self.width as f32 / scale, self.height as f32 / scale);
        // Transforms 1, 3, 5, 7 are 90/270 degree rotations.
        if self.transform % 2 == 1 { (h, w) } else { (w, h) }
    }
}

#[derive(Deserialize)]
struct HyprWorkspace {
    id: Option<i32>,
    #[serde(default)]
    hasfullscreen: bool,
}

#[derive(Deserialize)]
struct HyprClient {
    address: String,
    at: (i32, i32),
    size: (i32, i32),
    #[serde(default)]
    workspace: WorkspaceRef,
    #[serde(default = "default_true")]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct ActiveWorkspace {
    id: Option<i32>,
}

fn hyprctl<T: DeserializeOwned>(cmd: &str) -> Option<T> {
    let output = Command::new("hyprctl").args(["-j", cmd]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn get_monitors() -> Vec<HyprMonitor> {
    hyprctl::<Vec<HyprMonitor>>("monitors")
        .unwrap_or_default()
        .into_iter()
        .filter(|m| !m.disabled)
        .collect()
}

pub fn get_total_screen_bounds() -> (f32, f32, f32, f32) {
    let monitors = get_monitors();
    if monitors.is_empty() {
        return (0.0, 0.0, 1920.0, 1080.0);
    }

    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;

    for m in &monitors {
        let (w, h) = m.logical_size();
        min_x = min_x.min(m.x as f32);
        min_y = min_y.min(m.y as f32);
        max_x = max_x.max(m.x as f32 + w);
        max_y = max_y.max(m.y as f32 + h);
    }

    (min_x, min_y, max_x, max_y)
}

pub fn get_hyprland_windows() -> Vec<WindowRect> {
    let Some(active_workspace_id) =
        hyprctl::<ActiveWorkspace>("activeworkspace").and_then(|ws| ws.id)
    else {
        return Vec::new();
    };

    hyprctl::<Vec<HyprClient>>("clients")
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.mapped && !c.hidden && c.workspace.id == Some(active_workspace_id))
        .map(|c| WindowRect {
            address: c.address,
            x: c.at.0 as f32,
            y: c.at.1 as f32,
            width: c.size.0 as f32,
        })
        .collect()
}

pub fn get_monitors_with_fullscreen_state() -> Vec<MonitorRect> {
    let monitors = get_monitors();
    let workspaces = hyprctl::<Vec<HyprWorkspace>>("workspaces").unwrap_or_default();

    monitors
        .iter()
        .map(|monitor| {
            let has_fullscreen = monitor
                .active_workspace
                .id
                .and_then(|id| workspaces.iter().find(|w| w.id == Some(id)))
                .map(|w| w.hasfullscreen)
                .unwrap_or(false);

            let (width, height) = monitor.logical_size();
            MonitorRect {
                x: monitor.x as f32,
                y: monitor.y as f32,
                width,
                height,
                has_fullscreen,
            }
        })
        .collect()
}

pub fn spawn_event_listener() -> mpsc::Receiver<HyprlandEvent> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let mut event_listener = AsyncEventListener::new();

            let tx_clone = tx.clone();
            event_listener.add_window_opened_handler(move |_| {
                let tx = tx_clone.clone();
                Box::pin(async move {
                    let _ = tx.send(HyprlandEvent::WindowsChanged);
                })
            });

            let tx_clone = tx.clone();
            event_listener.add_window_closed_handler(move |_| {
                let tx = tx_clone.clone();
                Box::pin(async move {
                    let _ = tx.send(HyprlandEvent::WindowsChanged);
                })
            });

            let tx_clone = tx.clone();
            event_listener.add_window_moved_handler(move |_| {
                let tx = tx_clone.clone();
                Box::pin(async move {
                    let _ = tx.send(HyprlandEvent::WindowsChanged);
                })
            });

            let tx_clone = tx.clone();
            event_listener.add_active_window_changed_handler(move |_| {
                let tx = tx_clone.clone();
                Box::pin(async move {
                    let _ = tx.send(HyprlandEvent::WindowsChanged);
                })
            });

            let tx_clone = tx.clone();
            event_listener.add_workspace_changed_handler(move |_| {
                let tx = tx_clone.clone();
                Box::pin(async move {
                    let _ = tx.send(HyprlandEvent::WindowsChanged);
                })
            });

            let tx_clone = tx.clone();
            event_listener.add_fullscreen_state_changed_handler(move |_| {
                let tx = tx_clone.clone();
                Box::pin(async move {
                    let _ = tx.send(HyprlandEvent::WindowsChanged);
                })
            });

            let _ = event_listener.start_listener_async().await;
        });
    });

    rx
}

