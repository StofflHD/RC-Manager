//! Network functions from the original's context menu: Wake-on-LAN and reachability.

use anyhow::{anyhow, bail, Result};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

pub fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let hex: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        bail!("Invalid MAC address \"{mac}\"");
    }
    let mut out = [0u8; 6];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut p = vec![0xFF; 6];
    for _ in 0..16 {
        p.extend_from_slice(&mac);
    }
    p
}

pub fn wake_on_lan(mac: &str) -> Result<()> {
    let packet = magic_packet(parse_mac(mac)?);
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    for port in [9, 7] {
        sock.send_to(&packet, ("255.255.255.255", port))?;
    }
    Ok(())
}

/// TCP connect to the port; returns the response time.
pub fn check_port(host: &str, port: u16, timeout: Duration) -> Result<Duration> {
    let addrs: Vec<_> = (host.trim(), port).to_socket_addrs().map_err(|e| anyhow!("Cannot resolve {host}: {e}"))?.collect();
    let mut last = anyhow!("No address for {host}");
    for addr in addrs {
        let start = Instant::now();
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => return Ok(start.elapsed()),
            Err(e) => last = anyhow!("{addr}: {e}"),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_and_packet() {
        assert_eq!(parse_mac("00:1A-2b.3C4d5E").unwrap(), [0x00, 0x1A, 0x2B, 0x3C, 0x4D, 0x5E]);
        assert!(parse_mac("00:11").is_err());
        let p = magic_packet([1, 2, 3, 4, 5, 6]);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[96..], &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn port_check_localhost() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(check_port("127.0.0.1", port, Duration::from_secs(1)).is_ok());
        drop(listener);
    }
}
