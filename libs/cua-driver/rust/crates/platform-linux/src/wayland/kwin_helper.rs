//! KDE Plasma 6 / KWin exact-target foreground helper.
//!
//! This is an isolated prototype. It is intentionally not declared from
//! `wayland/mod.rs`; production Plasma support must not infer capability from
//! config records left by an unloaded script or an unavailable bridge.
//!
//! The helper contract is intentionally fail-closed:
//! - KWin owns the authoritative window list, active state, geometry, and
//!   stable `internalId`.
//! - The driver only trusts a versioned helper capability record plus a
//!   versioned snapshot written by the KWin-side script.
//! - Foreground activation is allowed only for one exact `(pid, window_id)`
//!   match and only after KWin confirms focus on that same window.
//! - The previously focused window must be restorable and is verified after the
//!   bounded foreground action completes.

use std::collections::HashSet;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::x11::WindowInfo;

const KWIN_HELPER_PROTOCOL_VERSION: u32 = 2;
const KWIN_HELPER_DEFAULT_CONFIG: &str = "kwinrc";
const KWIN_HELPER_DEFAULT_GROUP: &str = "Script-cua-kwin-helper";
const KWIN_HELPER_SERVICE: &str = "org.cua.KWinHelper";
const KWIN_HELPER_OBJECT: &str = "/org/cua/KWinHelper";
const KWIN_SCRIPTING_SERVICE: &str = "org.kde.KWin";
const KWIN_SCRIPTING_OBJECT: &str = "/Scripting";
const KWIN_HELPER_SCRIPT_ID: &str = "cua-kwin-helper";
const KEY_CAPABILITIES_JSON: &str = "capabilities_json";
const KEY_SNAPSHOT_JSON: &str = "snapshot_json";
const KEY_REQUEST_JSON: &str = "request_json";
const KEY_RESPONSE_JSON: &str = "response_json";
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(900);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub windows: Vec<WindowRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRecord {
    pub window_id: u64,
    pub pid: u32,
    pub title: String,
    pub app_id: String,
    pub resource_class: String,
    pub active: bool,
    pub minimized: bool,
    pub visible: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub stacking: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub compositor: String,
    pub can_snapshot: bool,
    pub can_activate_exact_window: bool,
    pub can_restore_focus: bool,
}

#[derive(Debug, Deserialize)]
struct RawCapabilities {
    version: u32,
    #[serde(default)]
    compositor: String,
    #[serde(default)]
    can_snapshot: bool,
    #[serde(default)]
    can_activate_exact_window: bool,
    #[serde(default)]
    can_restore_focus: bool,
}

#[derive(Debug, Deserialize)]
struct RawSnapshot {
    version: u32,
    windows: Vec<RawWindowRecord>,
}

#[derive(Debug, Deserialize)]
struct RawWindowRecord {
    window_id: u64,
    pid: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    app_id: String,
    #[serde(default)]
    resource_class: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    minimized: bool,
    #[serde(default)]
    visible: bool,
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    stacking: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ActivateRequest {
    version: u32,
    nonce: String,
    action: &'static str,
    pid: u32,
    window_id: u64,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum WireNonce {
    String(String),
    Number(u64),
}

impl WireNonce {
    fn as_string(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Number(value) => value.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ActivateResponse {
    version: u32,
    nonce: WireNonce,
    ok: bool,
    #[serde(default)]
    previous_window_id: Option<u64>,
    #[serde(default)]
    previous_pid: Option<u32>,
    #[serde(default)]
    active_window_id: Option<u64>,
    #[serde(default)]
    active_pid: Option<u32>,
    #[serde(default)]
    error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActivationProof {
    previous_window_id: Option<u64>,
    previous_pid: Option<u32>,
}

pub fn is_kwin_session() -> bool {
    is_kwin_session_from(
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var("XDG_SESSION_DESKTOP").ok().as_deref(),
        std::env::var("KDE_FULL_SESSION").ok().as_deref(),
    )
}

pub fn available() -> bool {
    bridge_is_live()
        && helper_script_is_loaded()
        && helper_capabilities().is_ok()
        && current_snapshot().is_ok()
}

fn bridge_is_live() -> bool {
    Command::new("qdbus")
        .args([KWIN_HELPER_SERVICE, KWIN_HELPER_OBJECT])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn helper_script_is_loaded() -> bool {
    let output = match Command::new("qdbus")
        .args([
            KWIN_SCRIPTING_SERVICE,
            KWIN_SCRIPTING_OBJECT,
            "isScriptLoaded",
            KWIN_HELPER_SCRIPT_ID,
        ])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    matches!(String::from_utf8_lossy(&output.stdout).trim(), "true" | "1")
}

pub fn list_windows(filter_pid: Option<u32>) -> Option<Vec<WindowInfo>> {
    if !available() {
        return None;
    }
    let snapshot = current_snapshot().ok()?;
    Some(
        snapshot
            .windows
            .into_iter()
            .filter(|window| filter_pid.is_none_or(|pid| window.pid == pid))
            .map(|window| WindowInfo {
                xid: window.window_id,
                pid: Some(window.pid),
                app_name: if window.app_id.is_empty() {
                    window.resource_class.clone()
                } else {
                    window.app_id.clone()
                },
                title: window.title,
                is_on_screen: window.visible
                    && !window.minimized
                    && window.width > 0
                    && window.height > 0,
                z_index: window.stacking,
                x: window.x,
                y: window.y,
                width: window.width,
                height: window.height,
            })
            .collect(),
    )
}

pub fn pid_for_window(window_id: u64) -> anyhow::Result<u32> {
    let snapshot = current_snapshot()?;
    let matches: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.window_id == window_id)
        .collect();
    match matches.as_slice() {
        [window] if window.pid > 0 => Ok(window.pid),
        [] => {
            anyhow::bail!("foreground_unavailable: KWin helper cannot prove window_id {window_id}")
        }
        [_] => anyhow::bail!(
            "foreground_unavailable: KWin helper reported pid 0 for window_id {window_id}"
        ),
        _ => anyhow::bail!(
            "foreground_unavailable: KWin helper reported ambiguous window_id {window_id}"
        ),
    }
}

pub fn geometry_for_window(window_id: u64) -> Option<(i32, i32, u32, u32)> {
    current_snapshot()
        .ok()?
        .windows
        .into_iter()
        .find_map(|window| {
            (window.window_id == window_id).then_some((
                window.x,
                window.y,
                window.width,
                window.height,
            ))
        })
}

pub fn activate_window(pid: u32, window_id: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        available(),
        "foreground_unavailable: KWin helper bridge/script is not live"
    );
    let capabilities = helper_capabilities()?;
    ensure_foreground_capabilities(&capabilities)?;
    let before = current_snapshot()?;
    resolve_exact_window(&before, pid, window_id)?;
    activate_and_verify(pid, window_id).map(|_| ())
}

pub fn parse_capabilities(raw: &str) -> anyhow::Result<Capabilities> {
    let payload = extract_json_object(raw, "KWin helper capabilities")?;
    let parsed: RawCapabilities =
        serde_json::from_str(payload).context("failed to decode KWin helper capabilities JSON")?;
    if parsed.version != KWIN_HELPER_PROTOCOL_VERSION {
        anyhow::bail!(
            "unsupported KWin helper protocol version {}; expected {}",
            parsed.version,
            KWIN_HELPER_PROTOCOL_VERSION
        );
    }
    if parsed.compositor.trim().is_empty() {
        anyhow::bail!("KWin helper did not identify its compositor");
    }
    Ok(Capabilities {
        compositor: parsed.compositor,
        can_snapshot: parsed.can_snapshot,
        can_activate_exact_window: parsed.can_activate_exact_window,
        can_restore_focus: parsed.can_restore_focus,
    })
}

pub fn parse_snapshot(raw: &str) -> anyhow::Result<Snapshot> {
    let payload = extract_json_object(raw, "KWin helper snapshot")?;
    let parsed: RawSnapshot =
        serde_json::from_str(payload).context("failed to decode KWin helper JSON")?;
    if parsed.version != KWIN_HELPER_PROTOCOL_VERSION {
        anyhow::bail!(
            "unsupported KWin helper protocol version {}; expected {}",
            parsed.version,
            KWIN_HELPER_PROTOCOL_VERSION
        );
    }

    let mut windows = Vec::with_capacity(parsed.windows.len());
    let mut ids = HashSet::with_capacity(parsed.windows.len());
    let mut active_count = 0usize;
    for window in parsed.windows {
        if window.pid == 0 {
            anyhow::bail!("KWin helper window {} reported pid 0", window.window_id);
        }
        if !ids.insert(window.window_id) {
            anyhow::bail!(
                "KWin helper reported duplicate window_id {}",
                window.window_id
            );
        }
        if window.active {
            active_count += 1;
        }
        windows.push(WindowRecord {
            window_id: window.window_id,
            pid: window.pid,
            title: window.title,
            app_id: window.app_id,
            resource_class: window.resource_class,
            active: window.active,
            minimized: window.minimized,
            visible: window.visible,
            x: window.x,
            y: window.y,
            width: window.width,
            height: window.height,
            stacking: window.stacking,
        });
    }
    if active_count > 1 {
        anyhow::bail!("KWin helper reported more than one active window");
    }
    Ok(Snapshot { windows })
}

fn extract_json_object<'a>(raw: &'a str, label: &str) -> anyhow::Result<&'a str> {
    let start = raw
        .find('{')
        .ok_or_else(|| anyhow::anyhow!("{label} contained no JSON object"))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| anyhow::anyhow!("{label} contained no JSON terminator"))?;
    Ok(&raw[start..=end])
}

fn helper_capabilities() -> anyhow::Result<Capabilities> {
    let raw = read_config_value(KEY_CAPABILITIES_JSON)
        .ok_or_else(|| anyhow::anyhow!("KWin helper capabilities are unavailable"))?;
    parse_capabilities(&raw)
}

fn current_snapshot() -> anyhow::Result<Snapshot> {
    let raw = read_config_value(KEY_SNAPSHOT_JSON)
        .ok_or_else(|| anyhow::anyhow!("KWin helper snapshot is unavailable"))?;
    parse_snapshot(&raw)
}

fn helper_config_file() -> String {
    std::env::var("CUA_KWIN_HELPER_CONFIG")
        .unwrap_or_else(|_| KWIN_HELPER_DEFAULT_CONFIG.to_owned())
}

fn helper_config_group() -> String {
    std::env::var("CUA_KWIN_HELPER_GROUP").unwrap_or_else(|_| KWIN_HELPER_DEFAULT_GROUP.to_owned())
}

fn read_config_value(key: &str) -> Option<String> {
    let output = Command::new("kreadconfig6")
        .arg("--file")
        .arg(helper_config_file())
        .arg("--group")
        .arg(helper_config_group())
        .arg("--key")
        .arg(key)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn write_config_value(key: &str, value: &str) -> anyhow::Result<()> {
    let status = Command::new("kwriteconfig6")
        .arg("--file")
        .arg(helper_config_file())
        .arg("--group")
        .arg(helper_config_group())
        .arg("--key")
        .arg(key)
        .arg(value)
        .status()
        .context("failed to launch kwriteconfig6 for the KWin helper")?;
    if !status.success() {
        anyhow::bail!("kwriteconfig6 failed while updating the KWin helper config");
    }

    // Legacy KWin JavaScript scripts do not receive a file-change watcher.
    // Their `options.configChanged` signal fires when KWin reconfigures, so
    // explicitly notify KWin after publishing a request. This is a local
    // session-bus method call; it does not inject input or change focus by
    // itself. The script performs the target/PID validation and focus proof.
    let reconfigure = Command::new("qdbus")
        .arg("org.kde.KWin")
        .arg("/KWin")
        .arg("reconfigure")
        .status()
        .context("failed to notify KWin after updating the helper config")?;
    if !reconfigure.success() {
        anyhow::bail!("KWin reconfigure failed after updating the helper config");
    }
    Ok(())
}

fn is_kwin_session_from(
    current_desktop: Option<&str>,
    session_desktop: Option<&str>,
    kde_full_session: Option<&str>,
) -> bool {
    kde_full_session.is_some_and(|value| !value.trim().is_empty() && value != "0")
        || [current_desktop, session_desktop]
            .into_iter()
            .flatten()
            .any(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("kde") || value.contains("plasma") || value.contains("kwin")
            })
}

fn resolve_exact_window(
    snapshot: &Snapshot,
    pid: u32,
    window_id: u64,
) -> anyhow::Result<WindowRecord> {
    let matches: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.pid == pid && window.window_id == window_id)
        .cloned()
        .collect();
    match matches.as_slice() {
        [] => anyhow::bail!(
            "KWin could not prove ownership of exact target window {window_id} for pid {pid}"
        ),
        [window] => Ok(window.clone()),
        _ => anyhow::bail!(
            "KWin reported more than one exact-target match for pid {pid} and window {window_id}"
        ),
    }
}

fn active_window(snapshot: &Snapshot) -> anyhow::Result<Option<WindowRecord>> {
    let active: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.active)
        .cloned()
        .collect();
    match active.as_slice() {
        [] => Ok(None),
        [window] => Ok(Some(window.clone())),
        _ => anyhow::bail!("KWin reported more than one active window"),
    }
}

fn ensure_foreground_capabilities(capabilities: &Capabilities) -> anyhow::Result<()> {
    if !capabilities
        .compositor
        .to_ascii_lowercase()
        .contains("kwin")
    {
        anyhow::bail!(
            "foreground_unavailable: expected a KWin helper, got compositor {:?}",
            capabilities.compositor
        );
    }
    if !capabilities.can_snapshot {
        anyhow::bail!("foreground_unavailable: the KWin helper cannot publish trusted snapshots");
    }
    if !capabilities.can_activate_exact_window {
        anyhow::bail!(
            "foreground_unavailable: the KWin helper cannot activate exact target windows"
        );
    }
    if !capabilities.can_restore_focus {
        anyhow::bail!(
            "foreground_unavailable: the KWin helper cannot restore the previously focused window"
        );
    }
    Ok(())
}

fn run_focus_transaction<T>(
    before: Snapshot,
    pid: u32,
    window_id: u64,
    mut activate: impl FnMut(u32, u64) -> anyhow::Result<ActivationProof>,
    mut snapshot_after_request: impl FnMut() -> anyhow::Result<Snapshot>,
    body: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let target = resolve_exact_window(&before, pid, window_id)?;
    let previous = active_window(&before)?
        .ok_or_else(|| anyhow::anyhow!("KWin did not expose a restorable focused window"))?;

    if previous.window_id == target.window_id {
        return body();
    }

    let activation = activate(target.pid, target.window_id)?;
    if activation.previous_window_id != Some(previous.window_id)
        || activation.previous_pid != Some(previous.pid)
    {
        anyhow::bail!(
            "KWin handled target activation after focus drift: helper displaced {:?}, \
             pid {:?}; expected previously focused window {} / pid {}",
            activation.previous_window_id,
            activation.previous_pid,
            previous.window_id,
            previous.pid
        );
    }
    let after_focus = snapshot_after_request()?;
    let focused_target = active_window(&after_focus)?.ok_or_else(|| {
        anyhow::anyhow!("KWin did not report any focused window after target activation")
    })?;
    if focused_target.window_id != target.window_id || focused_target.pid != target.pid {
        anyhow::bail!(
            "KWin did not confirm focus on exact target window {} / pid {} after activation",
            target.window_id,
            target.pid
        );
    }

    let body_result = body();
    let restore = activate(previous.pid, previous.window_id)?;
    if restore.previous_window_id != Some(target.window_id)
        || restore.previous_pid != Some(target.pid)
    {
        match body_result {
            Ok(_) => {
                anyhow::bail!(
                    "KWin lost focus on exact target window {} before restore; helper displaced {:?}",
                    target.window_id,
                    restore.previous_window_id
                );
            }
            Err(error) => {
                return Err(error.context(format!(
                    "KWin also lost focus on exact target window {} before restore; helper displaced {:?}",
                    target.window_id,
                    restore.previous_window_id
                )));
            }
        }
    }
    let after_restore = snapshot_after_request()?;
    let restored = active_window(&after_restore)?
        .ok_or_else(|| anyhow::anyhow!("KWin did not report any focused window after restore"))?;
    if restored.window_id != previous.window_id || restored.pid != previous.pid {
        match body_result {
            Ok(_) => {
                anyhow::bail!(
                    "KWin did not restore the previously focused window {} / pid {}",
                    previous.window_id,
                    previous.pid
                );
            }
            Err(error) => {
                return Err(error.context(format!(
                    "KWin also failed to restore the previously focused window {} / pid {}",
                    previous.window_id, previous.pid
                )));
            }
        }
    }
    body_result
}

fn next_nonce() -> String {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros() as u64)
        .unwrap_or(1);
    let current = NONCE.load(Ordering::Relaxed);
    if current == 0 {
        let _ = NONCE.compare_exchange(0, seed.max(1), Ordering::Relaxed, Ordering::Relaxed);
    }
    NONCE.fetch_add(1, Ordering::Relaxed).to_string()
}

fn activate_and_verify(pid: u32, window_id: u64) -> anyhow::Result<ActivationProof> {
    let nonce = next_nonce();
    let request = serde_json::to_string(&ActivateRequest {
        version: KWIN_HELPER_PROTOCOL_VERSION,
        nonce: nonce.clone(),
        action: "activate",
        pid,
        window_id,
    })
    .context("failed to serialize the KWin helper activate request")?;
    write_config_value(KEY_REQUEST_JSON, &request)?;

    let response = wait_for_response(&nonce)?;
    if !response.ok {
        anyhow::bail!(
            "KWin helper rejected activation for window {window_id}: {}",
            response.error
        );
    }
    if response.active_window_id != Some(window_id) {
        anyhow::bail!(
            "KWin helper reported focus on {:?}, not the requested window {}",
            response.active_window_id,
            window_id
        );
    }
    if response.active_pid != Some(pid) {
        anyhow::bail!(
            "KWin helper reported active pid {:?}, not the requested pid {}",
            response.active_pid,
            pid
        );
    }
    wait_for_active_window(pid, window_id)?;
    Ok(ActivationProof {
        previous_window_id: response.previous_window_id,
        previous_pid: response.previous_pid,
    })
}

fn wait_for_response(nonce: &str) -> anyhow::Result<ActivateResponse> {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        if let Some(raw) = read_config_value(KEY_RESPONSE_JSON) {
            let response: ActivateResponse = serde_json::from_str(&raw)
                .context("failed to decode the KWin helper response JSON")?;
            if response.version != KWIN_HELPER_PROTOCOL_VERSION {
                anyhow::bail!(
                    "unsupported KWin helper protocol version {}; expected {}",
                    response.version,
                    KWIN_HELPER_PROTOCOL_VERSION
                );
            }
            let response_nonce = response.nonce.as_string();
            if response_nonce == nonce {
                return Ok(response);
            }
            if response_nonce.is_empty() {
                anyhow::bail!(
                    "KWin helper reported an empty response nonce while waiting for request nonce {}",
                    nonce
                );
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!("KWin helper did not answer request nonce {nonce} in time");
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_active_window(pid: u32, window_id: u64) -> anyhow::Result<()> {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let snapshot = current_snapshot()?;
        if active_window(&snapshot)?
            .is_some_and(|window| window.window_id == window_id && window.pid == pid)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "KWin did not confirm focus on exact target window {} / pid {} after activation",
                window_id,
                pid
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

pub fn gate_exact_foreground_target(target_pid: Option<u32>, window_id: u64) -> anyhow::Result<()> {
    let pid = target_pid.ok_or_else(|| {
        anyhow::anyhow!(
            "foreground_unavailable: KDE Plasma 6/KWin requires a verified target pid \
             for window {window_id}; refusing global input because the exact compositor \
             target cannot be proven"
        )
    })?;
    let capabilities = helper_capabilities().map_err(|error| {
        anyhow::anyhow!(
            "foreground_unavailable: KDE Plasma 6/KWin detected, but the local helper \
             is unavailable or malformed ({error}). Install the KWin helper prototype \
             before using focus-bound/global input."
        )
    })?;
    ensure_foreground_capabilities(&capabilities)?;
    let snapshot = current_snapshot()?;
    resolve_exact_window(&snapshot, pid, window_id)?;
    let previous = active_window(&snapshot)?.ok_or_else(|| {
        anyhow::anyhow!("foreground_unavailable: KWin did not expose a restorable focused window")
    })?;
    if previous.window_id == window_id {
        return Ok(());
    }
    Ok(())
}

pub fn with_focused_window<T>(
    pid: u32,
    window_id: u64,
    body: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    gate_exact_foreground_target(Some(pid), window_id)?;
    let before = current_snapshot()?;
    run_focus_transaction(
        before,
        pid,
        window_id,
        |target_pid, target_window_id| activate_and_verify(target_pid, target_window_id),
        current_snapshot,
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities_json() -> String {
        r#"{"version":2,"compositor":"kwin_wayland","can_snapshot":true,"can_activate_exact_window":true,"can_restore_focus":true}"#
            .to_owned()
    }

    fn snapshot_json(windows: &str) -> String {
        format!("{{\"version\":2,\"windows\":[{windows}]}}")
    }

    fn window_json(window_id: u64, pid: u32, active: bool) -> String {
        format!(
            "{{\"window_id\":{window_id},\"pid\":{pid},\"title\":\"Window {window_id}\",\"app_id\":\"org.example.App\",\"resource_class\":\"example\",\"active\":{active},\"visible\":true,\"x\":10,\"y\":20,\"width\":300,\"height\":200,\"stacking\":1}}"
        )
    }

    #[test]
    fn parses_versioned_capabilities_response() {
        let parsed = parse_capabilities(&capabilities_json()).expect("valid capability JSON");
        assert_eq!(parsed.compositor, "kwin_wayland");
        assert!(parsed.can_snapshot);
        assert!(parsed.can_activate_exact_window);
        assert!(parsed.can_restore_focus);
    }

    #[test]
    fn parses_versioned_snapshot_response() {
        let parsed = parse_snapshot(&snapshot_json(&format!(
            "{},{}",
            window_json(7, 100, true),
            window_json(8, 200, false)
        )))
        .expect("valid KWin helper snapshot");
        assert_eq!(parsed.windows.len(), 2);
        assert_eq!(parsed.windows[0].window_id, 7);
        assert!(parsed.windows[0].active);
        assert_eq!(parsed.windows[1].pid, 200);
    }

    #[test]
    fn response_nonce_accepts_string_wire_format() {
        let response: ActivateResponse = serde_json::from_str(
            r#"{"version":2,"nonce":"1723571000-123-7","ok":true,"previous_window_id":44,"previous_pid":100,"active_window_id":55,"active_pid":200,"error":""}"#,
        )
        .expect("valid response JSON");
        assert_eq!(response.nonce.as_string(), "1723571000-123-7");
        assert_eq!(response.previous_window_id, Some(44));
        assert_eq!(response.previous_pid, Some(100));
        assert_eq!(response.active_window_id, Some(55));
        assert_eq!(response.active_pid, Some(200));
    }

    #[test]
    fn parser_rejects_duplicates_and_multiple_active_windows() {
        assert!(parse_snapshot(&snapshot_json(&format!(
            "{},{}",
            window_json(7, 100, false),
            window_json(7, 100, false)
        )))
        .is_err());
        assert!(parse_snapshot(&snapshot_json(&format!(
            "{},{}",
            window_json(7, 100, true),
            window_json(8, 200, true)
        )))
        .is_err());
    }

    #[test]
    fn exact_target_resolution_refuses_ambiguity() {
        let snapshot = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 7,
                    pid: 42,
                    title: "A".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 7,
                    pid: 42,
                    title: "B".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let error = resolve_exact_window(&snapshot, 42, 7)
            .unwrap_err()
            .to_string();
        assert!(error.contains("more than one exact-target match"));
    }

    #[test]
    fn focus_transaction_detects_focus_verification_failure() {
        let before = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 10,
                    pid: 99,
                    title: "Prior".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: true,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 20,
                    pid: 42,
                    title: "Target".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let after_focus = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 10,
                    pid: 99,
                    title: "Prior".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: true,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 20,
                    pid: 42,
                    title: "Target".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let error = run_focus_transaction(
            before,
            42,
            20,
            |_, _| {
                Ok(ActivationProof {
                    previous_window_id: Some(10),
                    previous_pid: Some(99),
                })
            },
            {
                let mut snapshots = vec![after_focus].into_iter();
                move || Ok(snapshots.next().expect("snapshot"))
            },
            || Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("did not confirm focus on exact target window 20"));
    }

    #[test]
    fn focus_transaction_detects_restore_failure() {
        let before = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 10,
                    pid: 99,
                    title: "Prior".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: true,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 20,
                    pid: 42,
                    title: "Target".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let after_focus = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 10,
                    pid: 99,
                    title: "Prior".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 20,
                    pid: 42,
                    title: "Target".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: true,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let after_restore = after_focus.clone();
        let error = run_focus_transaction(
            before,
            42,
            20,
            {
                let mut calls = 0usize;
                move |_, _| {
                    calls += 1;
                    Ok(ActivationProof {
                        previous_window_id: if calls == 1 { Some(10) } else { Some(20) },
                        previous_pid: if calls == 1 { Some(99) } else { Some(42) },
                    })
                }
            },
            {
                let mut snapshots = vec![after_focus, after_restore].into_iter();
                move || Ok(snapshots.next().expect("snapshot"))
            },
            || Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("did not restore the previously focused window 10"));
    }

    #[test]
    fn focus_transaction_rejects_helper_reported_focus_drift_before_activation() {
        let before = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 10,
                    pid: 99,
                    title: "Prior".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: true,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 20,
                    pid: 42,
                    title: "Target".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let error = run_focus_transaction(
            before,
            42,
            20,
            |_, _| {
                Ok(ActivationProof {
                    previous_window_id: Some(99),
                    previous_pid: Some(99),
                })
            },
            || unreachable!("activation mismatch should fail before requesting a snapshot"),
            || Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("handled target activation after focus drift"));
    }

    #[test]
    fn focus_transaction_rejects_helper_reported_focus_drift_before_restore() {
        let before = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 10,
                    pid: 99,
                    title: "Prior".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: true,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 20,
                    pid: 42,
                    title: "Target".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let after_focus = Snapshot {
            windows: vec![
                WindowRecord {
                    window_id: 10,
                    pid: 99,
                    title: "Prior".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: false,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
                WindowRecord {
                    window_id: 20,
                    pid: 42,
                    title: "Target".into(),
                    app_id: String::new(),
                    resource_class: String::new(),
                    active: true,
                    minimized: false,
                    visible: true,
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    stacking: None,
                },
            ],
        };
        let error = run_focus_transaction(
            before,
            42,
            20,
            {
                let mut calls = 0usize;
                move |_, _| {
                    calls += 1;
                    Ok(ActivationProof {
                        previous_window_id: if calls == 1 { Some(10) } else { Some(88) },
                        previous_pid: if calls == 1 { Some(99) } else { Some(88) },
                    })
                }
            },
            {
                let mut snapshots = vec![after_focus].into_iter();
                move || Ok(snapshots.next().expect("snapshot"))
            },
            || Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("lost focus on exact target window 20 before restore"));
    }

    #[test]
    fn list_windows_maps_snapshot_to_window_info() {
        let snapshot = parse_snapshot(&snapshot_json(&window_json(55, 77, true))).unwrap();
        let window = snapshot.windows.into_iter().next().unwrap();
        let info = WindowInfo {
            xid: window.window_id,
            pid: Some(window.pid),
            app_name: window.app_id.clone(),
            title: window.title.clone(),
            is_on_screen: window.visible && !window.minimized,
            z_index: window.stacking,
            x: window.x,
            y: window.y,
            width: window.width,
            height: window.height,
        };
        assert_eq!(info.xid, 55);
        assert_eq!(info.pid, Some(77));
        assert_eq!(info.app_name, "org.example.App");
    }

    #[test]
    fn foreground_capabilities_require_exact_activation_and_restore() {
        let mut capabilities = parse_capabilities(&capabilities_json()).unwrap();
        ensure_foreground_capabilities(&capabilities).unwrap();

        capabilities.can_restore_focus = false;
        let error = ensure_foreground_capabilities(&capabilities)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot restore the previously focused window"));
    }

    #[test]
    fn kwin_session_detection_matches_plasma_markers() {
        assert!(is_kwin_session_from(Some("KDE"), None, None));
        assert!(is_kwin_session_from(
            Some("ubuntu:GNOME"),
            Some("plasma"),
            None
        ));
        assert!(is_kwin_session_from(None, None, Some("true")));
        assert!(!is_kwin_session_from(Some("GNOME"), Some("mutter"), None));
    }
}
