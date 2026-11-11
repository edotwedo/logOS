//! Networking: our virtio-net driver under smoltcp's TCP/IP stack. DHCP for an address, DNS for
//! names, and a tiny HTTP/1.0 client so logOS can reach its boss brain by itself.
//! (Plain HTTP for now; TLS is on the roadmap. Nothing secret is sent.)

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::{dhcpv4, dns, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};

use crate::virtio_net::VirtioNet;

// ---------------------------------------------------------------- smoltcp device glue

pub struct Rx(Vec<u8>);
pub struct Tx<'a>(&'a mut VirtioNet);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        self.0.send(len, f)
    }
}

impl phy::Device for VirtioNet {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _t: Instant) -> Option<(Rx, Tx<'_>)> {
        let frame = self.recv(|f| f.to_vec())?;
        Some((Rx(frame), Tx(self)))
    }

    fn transmit(&mut self, _t: Instant) -> Option<Tx<'_>> {
        if self.can_send() { Some(Tx(self)) } else { None }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = 1514;
        c
    }
}

// ---------------------------------------------------------------- the network

enum Req {
    Idle,
    Resolving { q: dns::QueryHandle, body: String },
    Connecting { ip: Ipv4Address, body: String, sent: bool },
    Reading,
}

pub struct Net {
    dev: VirtioNet,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    dns: Option<SocketHandle>,
    tcp: SocketHandle,
    pub ip: Option<smoltcp::wire::Ipv4Cidr>,
    host: &'static str,
    path: &'static str,
    req: Req,
    resp: Vec<u8>,
    port: u16,
    started: u64,
}

impl Net {
    pub fn new(mut dev: VirtioNet, now_ms: u64, seed: u64, host: &'static str, path: &'static str) -> Net {
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(dev.mac)));
        config.random_seed = seed;
        let iface = Interface::new(config, &mut dev, Instant::from_millis(now_ms as i64));
        let mut sockets = SocketSet::new(vec![]);
        let dhcp = sockets.add(dhcpv4::Socket::new());
        let tcp_sock = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; 8192]), tcp::SocketBuffer::new(vec![0; 8192]));
        let tcp = sockets.add(tcp_sock);
        Net { dev, iface, sockets, dhcp, dns: None, tcp, ip: None, host, path, req: Req::Idle, resp: Vec::new(), started: 0, port: 49152 + (seed % 8000) as u16 }
    }

    pub fn ready(&self) -> bool {
        self.ip.is_some() && self.dns.is_some()
    }

    pub fn busy(&self) -> bool {
        !matches!(self.req, Req::Idle)
    }

    /// POST `body` (JSON) to the boss. The answer comes back from `poll` as protocol lines.
    pub fn ask(&mut self, body: String) -> bool {
        if !self.ready() || self.busy() {
            return false;
        }
        let Some(h) = self.dns else { return false };
        match self.sockets.get_mut::<dns::Socket>(h).start_query(self.iface.context(), self.host, DnsQueryType::A) {
            Ok(q) => {
                self.started = 0;
                self.req = Req::Resolving { q, body };
                true
            }
            Err(_) => false,
        }
    }

    /// Run the stack. Returns the boss's reply lines when a request finishes.
    pub fn poll(&mut self, now_ms: u64) -> Option<Vec<String>> {
        let now = Instant::from_millis(now_ms as i64);
        self.iface.poll(now, &mut self.dev, &mut self.sockets);

        // Copy what DHCP said out of the socket before touching the socket set again.
        let event = match self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            Some(dhcpv4::Event::Configured(cfg)) => {
                let servers: Vec<IpAddress> = cfg.dns_servers.iter().map(|s| IpAddress::Ipv4(*s)).collect();
                Some(Some((cfg.address, cfg.router, servers)))
            }
            Some(dhcpv4::Event::Deconfigured) => Some(None),
            None => None,
        };
        match event {
            Some(Some((address, router, servers))) => {
                self.iface.update_ip_addrs(|a| {
                    a.clear();
                    let _ = a.push(IpCidr::Ipv4(address));
                });
                if let Some(r) = router {
                    let _ = self.iface.routes_mut().add_default_ipv4_route(r);
                }
                log::info!("network up: address {} via {:?}, DNS {:?}", address, router, servers);
                if self.dns.is_none() {
                    self.dns = Some(self.sockets.add(dns::Socket::new(&servers, vec![])));
                }
                self.ip = Some(address);
            }
            Some(None) if self.ip.is_some() => {
                self.ip = None;
                log::info!("network down: DHCP lease lost");
            }
            _ => {}
        }

        // Give up on a request after 90 seconds instead of waiting forever.
        if self.busy() {
            if self.started == 0 {
                self.started = now_ms.max(1);
            } else if now_ms > self.started + 90_000 {
                self.sockets.get_mut::<tcp::Socket>(self.tcp).abort();
                self.req = Req::Idle;
                return Some(fail("the boss took too long to answer"));
            }
        }
        let req = core::mem::replace(&mut self.req, Req::Idle);
        self.req = match req {
            Req::Idle => Req::Idle,
            Req::Resolving { q, body } => {
                let h = self.dns.unwrap();
                match self.sockets.get_mut::<dns::Socket>(h).get_query_result(q) {
                    Ok(addrs) => match addrs.iter().find_map(|a| match a {
                        IpAddress::Ipv4(v4) => Some(*v4),
                        #[allow(unreachable_patterns)]
                        _ => None,
                    }) {
                        Some(ip) => {
                            self.port = if self.port >= 65000 { 49152 } else { self.port + 1 };
                            let s = self.sockets.get_mut::<tcp::Socket>(self.tcp);
                            s.abort();
                            match s.connect(self.iface.context(), (IpAddress::Ipv4(ip), 80), self.port) {
                                Ok(()) => Req::Connecting { ip, body, sent: false },
                                Err(_) => return Some(fail("could not open a connection")),
                            }
                        }
                        None => return Some(fail("DNS gave no IPv4 address")),
                    },
                    Err(dns::GetQueryResultError::Pending) => Req::Resolving { q, body },
                    Err(_) => return Some(fail("DNS lookup failed")),
                }
            }
            Req::Connecting { ip, body, sent } => {
                let s = self.sockets.get_mut::<tcp::Socket>(self.tcp);
                if !sent && s.may_send() {
                    let head = alloc::format!(
                        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nUser-Agent: logOS/0.7\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        self.path, self.host, body.len()
                    );
                    let _ = s.send_slice(head.as_bytes());
                    let _ = s.send_slice(body.as_bytes());
                    self.resp.clear();
                    Req::Reading
                } else if !s.is_open() {
                    return Some(fail("connection refused"));
                } else {
                    Req::Connecting { ip, body, sent }
                }
            }
            Req::Reading => {
                let s = self.sockets.get_mut::<tcp::Socket>(self.tcp);
                while s.can_recv() {
                    let resp = &mut self.resp;
                    let _ = s.recv(|d| {
                        resp.extend_from_slice(d);
                        (d.len(), ())
                    });
                }
                if !s.may_recv() || self.resp.len() > 64 * 1024 {
                    s.close();
                    let text = String::from_utf8_lossy(&self.resp).into_owned();
                    let body = text.split_once("\r\n\r\n").map(|(_, b)| String::from(b)).unwrap_or_default();
                    return Some(body.lines().map(String::from).collect());
                }
                Req::Reading
            }
        };
        None
    }
}

fn fail(why: &str) -> Vec<String> {
    vec![alloc::format!("@@SAY Network trouble: {}.", why), String::from("@@DONE")]
}
