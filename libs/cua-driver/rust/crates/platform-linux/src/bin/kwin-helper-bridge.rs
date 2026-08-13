//! Small session-bus publication bridge for the declarative KWin helper.
//!
//! This binary is an explicitly opt-in experimental target. It is not part of
//! the production Linux driver; the KWin declarative script cannot provide its
//! response publication path without this external session-bus service.
//!
//! KWin's script API can read its script config, but Plasma 6 does not expose
//! a writeConfig counterpart. The script therefore calls these three methods
//! through the user's session bus; this process validates the call locally and
//! writes the resulting JSON into the helper's existing kwinrc transport.

use std::process::Command;

use zbus::fdo;

const SERVICE: &str = "org.cua.KWinHelper";
const PATH: &str = "/org/cua/KWinHelper";
const DEFAULT_CONFIG: &str = "kwinrc";
const DEFAULT_GROUP: &str = "Script-cua-kwin-helper";

struct Bridge {
    config: String,
    group: String,
}

impl Bridge {
    fn publish(&self, key: &str, payload: &str) -> fdo::Result<()> {
        let output = Command::new("kwriteconfig6")
            .arg("--file")
            .arg(&self.config)
            .arg("--group")
            .arg(&self.group)
            .arg("--key")
            .arg(key)
            .arg(payload)
            .output()
            .map_err(|error| {
                fdo::Error::Failed(format!("failed to launch kwriteconfig6: {error}"))
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(fdo::Error::Failed(format!(
            "kwriteconfig6 failed for {key}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[zbus::interface(name = "org.cua.KWinHelper")]
impl Bridge {
    #[zbus(name = "PublishCapabilities")]
    fn publish_capabilities(&self, payload: &str) -> fdo::Result<()> {
        self.publish("capabilities_json", payload)
    }

    #[zbus(name = "PublishSnapshot")]
    fn publish_snapshot(&self, payload: &str) -> fdo::Result<()> {
        self.publish("snapshot_json", payload)
    }

    #[zbus(name = "PublishResponse")]
    fn publish_response(&self, payload: &str) -> fdo::Result<()> {
        self.publish("response_json", payload)
    }
}

#[tokio::main]
async fn main() -> zbus::Result<()> {
    let bridge = Bridge {
        config: std::env::var("CUA_KWIN_HELPER_CONFIG")
            .unwrap_or_else(|_| DEFAULT_CONFIG.to_owned()),
        group: std::env::var("CUA_KWIN_HELPER_GROUP").unwrap_or_else(|_| DEFAULT_GROUP.to_owned()),
    };
    let connection = zbus::connection::Builder::session()?
        .name(SERVICE)?
        .serve_at(PATH, bridge)?
        .build()
        .await?;
    tracing::info!(
        service = SERVICE,
        path = PATH,
        "KWin helper publication bridge is ready"
    );
    std::future::pending::<()>().await;
    drop(connection);
    Ok(())
}
