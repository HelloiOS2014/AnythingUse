//! Chrome control-plane readiness probe (socket + host + extension).

use crate::ChromeControlClient;

/// Socket present + host_ping (host process). Extension `ping` is separate.
pub fn probe_chrome(client: &ChromeControlClient) -> ChromeProbe {
    let sock = client.sock_path().display().to_string();
    if !client.socket_present() {
        return ChromeProbe {
            socket_present: false,
            host_ok: false,
            extension_ok: false,
            note: format!(
                "chrome_tab offline: socket missing at {sock}; install native host + load LCU Chrome Control extension"
            ),
        };
    }
    let host_ok = client.host_ping().is_ok();
    let extension_ok = host_ok && client.ping().is_ok();
    let note = if extension_ok {
        format!("chrome_tab connected sock={sock} host=ok extension=ok")
    } else if host_ok {
        format!(
            "chrome_tab host up at {sock} but extension not responding (load unpacked native/chrome-control/extension)"
        )
    } else {
        format!(
            "chrome_tab socket present at {sock} but host_ping failed (Chrome must launch native host via extension)"
        )
    };
    ChromeProbe {
        socket_present: true,
        host_ok,
        extension_ok,
        note,
    }
}

#[derive(Debug, Clone)]
pub struct ChromeProbe {
    pub socket_present: bool,
    pub host_ok: bool,
    pub extension_ok: bool,
    pub note: String,
}
