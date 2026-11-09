//! Finding and installing new versions.
//!
//! The latest release comes from GitHub. The Flatpak installs it through
//! Flatpak's update portal (`org.freedesktop.portal.Flatpak`), which pulls
//! it from the repository the app was installed from and needs no extra
//! permission; native installs are updated by the package manager, so they
//! only point at the release.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio::prelude::*;
use gtk::{gio, glib};
use serde::Deserialize;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_URL: &str = "https://github.com/Dukko/DrukhalManager/releases";
const LATEST_API: &str = "https://api.github.com/repos/Dukko/DrukhalManager/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Without the leading `v`, e.g. `0.1.2`.
    pub version: String,
    /// Markdown, as written on GitHub.
    pub notes: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
}

/// The latest published release. Blocks; call off the main thread.
pub fn latest() -> Result<Release, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let mut response = agent
        .get(LATEST_API)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", &format!("DrukhalManager/{CURRENT}"))
        .call()
        .map_err(|e| format!("Could not reach GitHub: {e}"))?;
    let release: GithubRelease = response.body_mut().read_json().map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    Ok(Release {
        version: release.tag_name.trim_start_matches('v').to_owned(),
        notes: release.body.unwrap_or_default().trim().to_owned(),
        url: release.html_url,
    })
}

/// Whether `candidate` is a later version than `current`, comparing the
/// dot-separated numbers (`0.10.0` > `0.9.3`). Anything unparsable is not
/// newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Option<Vec<u64>> {
        v.trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
    }
    match (parts(candidate), parts(current)) {
        (Some(a), Some(b)) => {
            let len = a.len().max(b.len());
            let pad = |v: Vec<u64>| v.into_iter().chain(std::iter::repeat(0)).take(len).collect::<Vec<_>>();
            pad(a) > pad(b)
        }
        _ => false,
    }
}

/// Where an update stands, as the portal reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Percent done.
    Running(u32),
    /// Already up to date.
    NothingToDo,
    Done,
    Failed(String),
}

const PORTAL_NAME: &str = "org.freedesktop.portal.Flatpak";
const PORTAL_PATH: &str = "/org/freedesktop/portal/Flatpak";

type Slot<F> = Rc<RefCell<Option<Box<F>>>>;

/// Flatpak's update monitor for this app. It reports newly published
/// versions and installs them.
pub struct Monitor {
    proxy: gio::DBusProxy,
    on_available: Slot<dyn Fn()>,
    on_progress: Slot<dyn Fn(Progress)>,
}

impl Monitor {
    pub async fn new() -> Result<Self, glib::Error> {
        let portal = gio::DBusProxy::for_bus_future(gio::BusType::Session, gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES, None, PORTAL_NAME, PORTAL_PATH, PORTAL_NAME).await?;
        let options = glib::VariantDict::new(None).end();
        let reply = portal.call_future("CreateUpdateMonitor", Some(&(options,).into()), gio::DBusCallFlags::NONE, -1).await?;
        let (path,) = reply.get::<(glib::variant::ObjectPath,)>().ok_or_else(|| glib::Error::new(gio::IOErrorEnum::InvalidData, "unexpected reply"))?;
        let proxy = gio::DBusProxy::for_bus_future(
            gio::BusType::Session,
            gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
            None,
            PORTAL_NAME,
            path.as_str(),
            "org.freedesktop.portal.Flatpak.UpdateMonitor",
        )
        .await?;
        let monitor = Self { proxy, on_available: Default::default(), on_progress: Default::default() };
        let (on_available, on_progress) = (monitor.on_available.clone(), monitor.on_progress.clone());
        monitor.proxy.connect_local("g-signal", false, move |values| {
            let signal: String = values[2].get().ok()?;
            let params: glib::Variant = values[3].get().ok()?;
            let info = params.try_child_value(0).map(|v| glib::VariantDict::new(Some(&v)));
            match signal.as_str() {
                "UpdateAvailable" => {
                    if let Some(f) = &*on_available.borrow() {
                        f();
                    }
                }
                "Progress" => {
                    if let (Some(info), Some(f)) = (info, &*on_progress.borrow()) {
                        f(parse_progress(&info));
                    }
                }
                _ => {}
            }
            None
        });
        Ok(monitor)
    }

    /// Called when Flatpak notices a newer version in the app's remote.
    pub fn on_available(&self, f: impl Fn() + 'static) {
        self.on_available.replace(Some(Box::new(f)));
    }

    pub fn on_progress(&self, f: impl Fn(Progress) + 'static) {
        self.on_progress.replace(Some(Box::new(f)));
    }

    /// Starts installing the latest version; progress arrives through
    /// `on_progress`.
    pub async fn update(&self) -> Result<(), glib::Error> {
        let options = glib::VariantDict::new(None).end();
        self.proxy.call_future("Update", Some(&("", options).into()), gio::DBusCallFlags::NONE, -1).await.map(|_| ())
    }
}

fn parse_progress(info: &glib::VariantDict) -> Progress {
    let status = info.lookup::<u32>("status").ok().flatten().unwrap_or(0);
    match status {
        1 => Progress::NothingToDo,
        2 => Progress::Done,
        3 => Progress::Failed(
            info.lookup::<String>("error_message")
                .ok()
                .flatten()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "The update failed".to_owned()),
        ),
        _ => {
            // `progress` is per operation; spread it over all of them.
            let get = |key: &str| info.lookup::<u32>(key).ok().flatten();
            let (n_ops, op, progress) = (get("n_ops").unwrap_or(1).max(1), get("op").unwrap_or(0), get("progress").unwrap_or(0).min(100));
            Progress::Running(((op.min(n_ops - 1) * 100 + progress) / n_ops).min(100))
        }
    }
}

/// Starts the installed (just updated) version through the Flatpak portal.
/// The caller then quits; the new process waits a moment so it doesn't
/// hand itself over to this one.
pub async fn relaunch() -> Result<(), glib::Error> {
    const LATEST_VERSION: u32 = 2;
    let portal = gio::DBusProxy::for_bus_future(gio::BusType::Session, gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES, None, PORTAL_NAME, PORTAL_PATH, PORTAL_NAME).await?;
    let bytes = |s: &str| {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        b
    };
    let argv: Vec<Vec<u8>> = ["sh", "-c", "sleep 1; exec drukhal-manager"].iter().map(|s| bytes(s)).collect();
    let fds: std::collections::HashMap<u32, glib::variant::Handle> = Default::default();
    let envs: std::collections::HashMap<String, String> = Default::default();
    let params = glib::Variant::tuple_from_iter([
        bytes("/").to_variant(),
        argv.to_variant(),
        fds.to_variant(),
        envs.to_variant(),
        LATEST_VERSION.to_variant(),
        glib::VariantDict::new(None).end(),
    ]);
    portal.call_future("Spawn", Some(&params), gio::DBusCallFlags::NONE, -1).await.map(|_| ())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.1.0", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(!is_newer("nightly", "0.1.1"));
    }

    #[test]
    fn reads_portal_progress() {
        let dict = |pairs: &[(&str, u32)]| {
            let d = glib::VariantDict::new(None);
            for (k, v) in pairs {
                d.insert(k, v);
            }
            d
        };
        assert_eq!(parse_progress(&dict(&[("status", 0), ("n_ops", 2), ("op", 1), ("progress", 50)])), Progress::Running(75));
        assert_eq!(parse_progress(&dict(&[("status", 2)])), Progress::Done);
        assert_eq!(parse_progress(&dict(&[("status", 1)])), Progress::NothingToDo);
        let failed = glib::VariantDict::new(None);
        failed.insert("status", 3u32);
        failed.insert("error_message", "no space");
        assert_eq!(parse_progress(&failed), Progress::Failed("no space".into()));
    }
}
