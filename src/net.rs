//! Reaching a Model Roamer server: the addresses this machine answers on (LAN, Tailscale, WireGuard, hostnames, a
//! domain), finding servers on the local network, and QR codes for pairing a phone.

use crate::{core::Core, procs};
use serde_json::{json, Value};
use std::{net::IpAddr, time::Duration};

pub const DISCOVERY_PORT: u16 = 4631;

fn kind_of(name: &str, ip: &IpAddr) -> &'static str {
    let n = name.to_lowercase();
    if ip.is_loopback() {
        return "loopback";
    }
    if n.contains("tailscale") || n.starts_with("utun") && is_cgnat(ip) || is_cgnat(ip) && n.contains("ts") {
        return "tailscale";
    }
    if is_cgnat(ip) {
        return "tailscale";
    }
    if n.starts_with("wg") || n.contains("wireguard") || n.contains("wintun") && !n.contains("tailscale") {
        return "wireguard";
    }
    match ip {
        IpAddr::V4(v4) if v4.is_private() => "lan",
        IpAddr::V6(v6) if (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80 => "lan",
        _ => "public",
    }
}

/// 100.64.0.0/10, the range Tailscale hands out.
fn is_cgnat(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v) if v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]))
}

pub fn interfaces() -> Vec<Value> {
    let mut out = vec![];
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        let ip = i.ip();
        if let IpAddr::V6(v6) = ip {
            if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                continue; // link-local v6 needs a zone id; not useful as a URL
            }
        }
        out.push(json!({ "name": i.name, "ip": ip.to_string(), "kind": kind_of(&i.name, &ip), "v6": ip.is_ipv6() }));
    }
    out
}

/// Tailscale's view of this machine: its MagicDNS name and tailnet addresses, when Tailscale is installed.
pub async fn tailscale() -> Value {
    let Some(ts) = procs::resolve(&["tailscale"], None) else { return json!({ "installed": false }) };
    let out = procs::run(&ts, &procs::Launch { args: vec!["status".into(), "--json".into()], ..Default::default() }, Duration::from_secs(10)).await;
    match out {
        Ok(o) if o.ok() => {
            let v: Value = serde_json::from_str(&o.stdout).unwrap_or_default();
            let me = &v["Self"];
            json!({
                "installed": true,
                "running": v["BackendState"] == "Running",
                "state": v["BackendState"],
                "dns": me["DNSName"].as_str().unwrap_or("").trim_end_matches('.'),
                "ips": me["TailscaleIPs"],
                "tailnet": v["CurrentTailnet"]["Name"],
                "https": v["CertDomains"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
            })
        }
        Ok(o) => json!({ "installed": true, "running": false, "error": o.text().trim() }),
        Err(e) => json!({ "installed": true, "running": false, "error": e.to_string() }),
    }
}

/// Give this server an HTTPS address on the tailnet (`https://machine.tailnet.ts.net`), via `tailscale serve`.
pub async fn tailscale_serve(port: u16, on: bool) -> anyhow::Result<String> {
    let ts = procs::resolve(&["tailscale"], None).ok_or_else(|| anyhow::anyhow!("Tailscale isn't installed."))?;
    let args: Vec<String> = if on { vec!["serve".into(), "--bg".into(), format!("http://127.0.0.1:{port}")] } else { vec!["serve".into(), "reset".into()] };
    let o = procs::run(&ts, &procs::Launch { args, ..Default::default() }, Duration::from_secs(30)).await?;
    if !o.ok() {
        anyhow::bail!("{}", o.text().trim());
    }
    Ok(o.text())
}

/// Every URL other devices could use to reach this server, best first.
pub async fn addresses(core: &Core, port: u16, tls: bool) -> Value {
    let scheme = if tls { "https" } else { "http" };
    let mut urls = vec![];
    let public = core.settings()["server"]["public_url"].as_str().unwrap_or("").trim().trim_end_matches('/').to_string();
    if !public.is_empty() {
        urls.push(json!({ "via": "domain", "label": "Your domain", "url": public }));
    }
    let ts = tailscale().await;
    if ts["running"] == true {
        let dns = ts["dns"].as_str().unwrap_or("");
        if !dns.is_empty() {
            urls.push(json!({ "via": "tailscale", "label": "Tailscale (MagicDNS)", "url": format!("{scheme}://{dns}:{port}") }));
        }
    }
    let ifs = interfaces();
    for k in ["tailscale", "wireguard", "lan", "public"] {
        for i in ifs.iter().filter(|i| i["kind"] == k) {
            let ip = i["ip"].as_str().unwrap_or("");
            let host = if i["v6"] == true { format!("[{ip}]") } else { ip.to_string() };
            let label = match k {
                "tailscale" => "Tailscale",
                "wireguard" => "WireGuard",
                "lan" => "Local network",
                _ => "Public address",
            };
            urls.push(json!({ "via": k, "label": format!("{label} ({})", i["name"].as_str().unwrap_or("")), "url": format!("{scheme}://{host}:{port}") }));
        }
    }
    let host = crate::os::machine_name();
    urls.push(json!({ "via": "host", "label": "Hostname", "url": format!("{scheme}://{host}:{port}") }));
    urls.push(json!({ "via": "host", "label": "Hostname (.local)", "url": format!("{scheme}://{}.local:{port}", host.to_lowercase()) }));
    json!({ "urls": urls, "interfaces": ifs, "tailscale": ts, "hostname": host })
}

/// Tell the local network a server is here, every few seconds (desktops find it under Connect › Find).
pub async fn announce(core: crate::core::CoreRef, port: u16, tls: bool) {
    let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else { return };
    let _ = sock.set_broadcast(true);
    loop {
        if core.settings()["server"]["discoverable"] != false {
            let msg = json!({ "app": "model-roamer", "id": core.instance_id, "name": core.display_name(), "port": port, "tls": tls, "version": env!("CARGO_PKG_VERSION") });
            let _ = sock.send_to(msg.to_string().as_bytes(), ("255.255.255.255", DISCOVERY_PORT)).await;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Listen for servers announcing themselves for a few seconds.
pub async fn discover(wait: Duration) -> Vec<Value> {
    let Ok(sock) = tokio::net::UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)).await else {
        return vec![json!({ "error": "Couldn't listen for servers (is another Model Roamer looking right now?)" })];
    };
    let mut found: Vec<Value> = vec![];
    let end = tokio::time::Instant::now() + wait;
    let mut buf = [0u8; 2048];
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(end, sock.recv_from(&mut buf)).await {
        if let Ok(mut v) = serde_json::from_slice::<Value>(&buf[..n]) {
            if v["app"] != "model-roamer" {
                continue;
            }
            let ip = from.ip();
            let host = if ip.is_ipv6() { format!("[{ip}]") } else { ip.to_string() };
            v["url"] = json!(format!("{}://{}:{}", if v["tls"] == true { "https" } else { "http" }, host, v["port"]));
            if !found.iter().any(|f| f["id"] == v["id"]) {
                found.push(v);
            }
        }
    }
    found
}

pub fn qr_svg(text: &str) -> String {
    use qrcode::{render::svg, QrCode};
    match QrCode::new(text.as_bytes()) {
        Ok(c) => c.render::<svg::Color>().min_dimensions(180, 180).quiet_zone(true).dark_color(svg::Color("#0a1113")).light_color(svg::Color("#f2f6ea")).build(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies() {
        assert_eq!(kind_of("tailscale0", &"100.101.102.103".parse().unwrap()), "tailscale");
        assert_eq!(kind_of("wg0", &"10.8.0.2".parse().unwrap()), "wireguard");
        assert_eq!(kind_of("eth0", &"192.168.1.20".parse().unwrap()), "lan");
        assert_eq!(kind_of("lo", &"127.0.0.1".parse().unwrap()), "loopback");
        assert!(qr_svg("http://x").contains("<svg"));
    }
}
