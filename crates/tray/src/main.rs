//! Desktop tray helper for the `OpenATC` AI server manager.
//!
//! The server starts this only when a desktop session is available. It reads
//! the manager status over the dashboard HTTP API and offers the same actions
//! as the web page.

#[cfg(target_os = "linux")]
use ksni::blocking::TrayMethods;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
struct Tray {
    url: String,
    release: String,
    status: String,
    detail: String,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "openatc-ai".to_owned()
    }

    fn icon_name(&self) -> String {
        "applications-system".to_owned()
    }

    fn title(&self) -> String {
        format!("OpenATC AI · {}", self.status)
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        vec![
            StandardItem {
                label: format!("Status: {}", self.status),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.detail.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.release.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Hara-kiri".to_owned(),
                icon_name: "application-exit".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    post(&tray.url, "shutdown");
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit tray".to_owned(),
                icon_name: "window-close".to_owned(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
fn main() {
    if std::env::args().any(|arg| arg == "--help") {
        println!("Usage: openatc-tray --url http://HOST:PORT --release NAME");
        return;
    }
    let mut url = String::new();
    let mut release = String::from("OpenATC AI");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => url = args.next().unwrap_or_default(),
            "--release" => release = args.next().unwrap_or(release),
            _ => {}
        }
    }
    if url.is_empty() {
        eprintln!("openatc-tray: --url is required");
        std::process::exit(2);
    }
    let (status, detail) = fetch_status(&url);
    let poll_url = url.clone();
    let mut current_status = status.clone();
    let mut current_detail = detail.clone();
    let tray = Tray {
        url,
        release,
        status,
        detail,
    };
    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("openatc-tray: {error}");
            return;
        }
    };
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let (status, detail) = fetch_status(&poll_url);
        if status != current_status || detail != current_detail {
            let _ = handle.update(|tray: &mut Tray| {
                if tray.status == status && tray.detail == detail {
                    return false;
                }
                tray.status.clone_from(&status);
                tray.detail.clone_from(&detail);
                true
            });
            current_status = status;
            current_detail = detail;
        }
    }
}

#[cfg(target_os = "linux")]
fn fetch_status(url: &str) -> (String, String) {
    let Ok(response) = ureq::get(format!("{url}/status")).call() else {
        return (
            "unreachable".to_owned(),
            "Dashboard not reachable".to_owned(),
        );
    };
    let mut response = response;
    let Ok(text) = response.body_mut().read_to_string() else {
        return (
            "unreachable".to_owned(),
            "Dashboard not reachable".to_owned(),
        );
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (
            "unreachable".to_owned(),
            "Dashboard returned invalid status".to_owned(),
        );
    };
    let stage = value
        .get("runtime")
        .and_then(|runtime| runtime.get("stage"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let inference = value
        .get("runtime")
        .and_then(|runtime| runtime.get("health"))
        .and_then(|health| health.get("version"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("not running");
    (stage.to_owned(), format!("Inference: {inference}"))
}

#[cfg(target_os = "linux")]
fn post(url: &str, action: &str) {
    let _ = ureq::post(format!("{url}/action/{action}"))
        .header("Origin", url)
        .header("Content-Type", "application/json")
        .send_empty();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("openatc-tray: this platform currently uses the web dashboard");
}
