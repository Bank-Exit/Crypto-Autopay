//! "No network at all" detection, without contacting anyone.
//!
//! Connecting a UDP socket only asks the OS for a route: no packet is sent. With every
//! interface down (cable out, Wi-Fi off) there is no route and the call fails at once,
//! which tells "no network" apart from "network up but Monero nodes unreachable".

use std::net::{IpAddr, UdpSocket};

/// Error prefix the frontend recognises.
pub const OFFLINE: &str = "OFFLINE: no network connection";

/// Whether the OS has a route towards the Internet (IPv4 or IPv6).
pub fn has_route() -> bool {
    // Documentation addresses (RFC 5737 / RFC 3849): looked up in the routing table only.
    let v4 = UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect("192.0.2.1:9"));
    let v6 = UdpSocket::bind("[::]:0").and_then(|s| s.connect("[2001:db8::1]:9"));
    v4.is_ok() || v6.is_ok()
}

/// A node on this machine or on the local network can answer without Internet access.
pub fn is_local_node(addr: &str) -> bool {
    let rest = addr.split_once("://").map_or(addr, |(_, r)| r);
    let host_port = rest.split('/').next().unwrap_or("");
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host_port.rsplit_once(':').map_or(host_port, |(h, _)| h)
    };
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// True when syncing is pointless: no route at all and no local node configured.
pub fn offline(configured_node: Option<&str>) -> bool {
    !configured_node.is_some_and(is_local_node) && !has_route()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_nodes() {
        assert!(is_local_node("http://127.0.0.1:18081"));
        assert!(is_local_node("localhost:18081"));
        assert!(is_local_node("192.168.1.20:18089"));
        assert!(is_local_node("http://[::1]:18081"));
        assert!(is_local_node("node.local:18081"));
        assert!(!is_local_node("http://node.moneroworld.com:18089"));
        assert!(!is_local_node("http://88.198.1.2:18081"));
    }
}
