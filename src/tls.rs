//! TLS for the uplink: rustls (TLS 1.2/1.3, Mozilla's root store) on pure-Rust crypto.
//!
//! logOS has no OS underneath, so three things rustls normally borrows from one are built here:
//! - randomness: RDRAND when the CPU has it, mixed with TSC jitter, stretched through SHA-256;
//! - the wall clock: the CMOS real-time clock read once, then advanced by the PIT tick count;
//! - the socket: `Tls` never touches the network. `net.rs` feeds it ciphertext and sends what it returns.

use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use rustls::client::UnbufferedClientConnection;
use rustls::pki_types::{ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};
use rustls::{ClientConfig, RootCertStore};
use sha2::{Digest, Sha256};

use crate::arch;

// ---------------------------------------------------------------- randomness

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn has_rdrand() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0
}

fn rdrand() -> Option<u64> {
    for _ in 0..10 {
        let v: u64;
        let ok: u8;
        unsafe { core::arch::asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack)) };
        if ok != 0 {
            return Some(v);
        }
    }
    None
}

static POOL: spin_pool::Pool = spin_pool::Pool::new();

mod spin_pool {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    /// A 32-byte entropy pool behind a spin lock (logOS has one core and no threads,
    /// but the timer interrupt must never see a half-written pool).
    pub struct Pool {
        lock: AtomicBool,
        state: UnsafeCell<([u8; 32], u64)>,
    }
    unsafe impl Sync for Pool {}
    impl Pool {
        pub const fn new() -> Pool {
            Pool { lock: AtomicBool::new(false), state: UnsafeCell::new(([0; 32], 0)) }
        }
        pub fn with<R>(&self, f: impl FnOnce(&mut [u8; 32], &mut u64) -> R) -> R {
            while self.lock.swap(true, Ordering::Acquire) {
                core::hint::spin_loop();
            }
            let s = unsafe { &mut *self.state.get() };
            let r = f(&mut s.0, &mut s.1);
            self.lock.store(false, Ordering::Release);
            r
        }
    }
}

/// Gather entropy into the pool. Called once at boot; every draw also mixes in fresh TSC/RDRAND.
pub fn seed(extra: u64) {
    let mut h = Sha256::new();
    h.update(extra.to_le_bytes());
    let rd = has_rdrand();
    // TSC jitter: time a few thousand tiny, uneven loops. On real hardware and in QEMU alike
    // the low bits of these deltas wobble with caches, interrupts and the host scheduler.
    let mut last = rdtsc();
    for i in 0..4096u64 {
        let mut x = i;
        for _ in 0..(i & 31) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        let now = rdtsc();
        h.update((now.wrapping_sub(last) ^ x).to_le_bytes());
        last = now;
        if rd && i % 64 == 0 {
            if let Some(r) = rdrand() {
                h.update(r.to_le_bytes());
            }
        }
    }
    let out = h.finalize();
    POOL.with(|key, _| key.copy_from_slice(out.as_slice()));
    log::info!("entropy pool seeded ({})", if rd { "RDRAND + TSC jitter" } else { "TSC jitter only, no RDRAND on this CPU" });
}

/// Fill `dest` with random bytes: SHA-256(key || counter || tsc || rdrand), then ratchet the key.
pub fn fill(dest: &mut [u8]) {
    let rd = has_rdrand();
    POOL.with(|key, ctr| {
        for chunk in dest.chunks_mut(32) {
            *ctr += 1;
            let mut h = Sha256::new();
            h.update(&key[..]);
            h.update(ctr.to_le_bytes());
            h.update(rdtsc().to_le_bytes());
            if rd {
                if let Some(r) = rdrand() {
                    h.update(r.to_le_bytes());
                }
            }
            let block = h.finalize();
            chunk.copy_from_slice(&block.as_slice()[..chunk.len()]);
        }
        let mut h = Sha256::new();
        h.update(b"ratchet");
        h.update(&key[..]);
        h.update(ctr.to_le_bytes());
        key.copy_from_slice(h.finalize().as_slice());
    });
}

#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(dest: *mut u8, len: usize) -> Result<(), getrandom::Error> {
    fill(unsafe { core::slice::from_raw_parts_mut(dest, len) });
    Ok(())
}

// ---------------------------------------------------------------- the clock

static BOOT_UNIX: AtomicU64 = AtomicU64::new(0);
static BOOT_TICKS: AtomicU64 = AtomicU64::new(0);

fn cmos(reg: u8) -> u8 {
    arch::outb(0x70, reg);
    arch::inb(0x71)
}

fn rtc_snapshot() -> [u8; 7] {
    while cmos(0x0A) & 0x80 != 0 {}
    [cmos(0x00), cmos(0x02), cmos(0x04), cmos(0x07), cmos(0x08), cmos(0x09), cmos(0x32)]
}

/// Read the CMOS clock (assumed UTC, which QEMU and Linux-installed machines use) as Unix seconds.
pub fn read_rtc() -> u64 {
    // Read until two snapshots agree, so we never catch it mid-update.
    let mut a = rtc_snapshot();
    loop {
        let b = rtc_snapshot();
        if a == b {
            break;
        }
        a = b;
    }
    let status_b = cmos(0x0B);
    let bcd = |v: u8| if status_b & 0x04 == 0 { (v & 0x0F) + (v >> 4) * 10 } else { v };
    let sec = bcd(a[0]) as u64;
    let min = bcd(a[1]) as u64;
    let mut hour = bcd(a[2] & 0x7F) as u64;
    if status_b & 0x02 == 0 && a[2] & 0x80 != 0 {
        hour = (hour % 12) + 12; // 12-hour clock, PM bit set
    }
    let day = bcd(a[3]) as i64;
    let month = bcd(a[4]) as i64;
    let century = match bcd(a[6]) {
        c @ 19..=21 => c as i64,
        _ => 20,
    };
    let year = century * 100 + bcd(a[5]) as i64;
    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    (days.max(0) as u64) * 86400 + hour * 3600 + min * 60 + sec
}

pub fn clock_init() {
    let now = read_rtc();
    BOOT_UNIX.store(now, Ordering::Relaxed);
    BOOT_TICKS.store(arch::TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    log::info!("wall clock from CMOS: unix {}", now);
}

pub fn unix_now() -> u64 {
    let ticks = arch::TICKS.load(Ordering::Relaxed) - BOOT_TICKS.load(Ordering::Relaxed);
    BOOT_UNIX.load(Ordering::Relaxed) + ticks / arch::TICK_HZ
}

#[derive(Debug)]
struct RtcClock;

impl TimeProvider for RtcClock {
    fn current_time(&self) -> Option<UnixTime> {
        match BOOT_UNIX.load(Ordering::Relaxed) {
            0 => None,
            _ => Some(UnixTime::since_unix_epoch(core::time::Duration::from_secs(unix_now()))),
        }
    }
}

// ---------------------------------------------------------------- the session

static CONFIG: spin_once::Once<Arc<ClientConfig>> = spin_once::Once::new();

mod spin_once {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};
    pub struct Once<T> {
        set: AtomicBool,
        v: UnsafeCell<Option<T>>,
    }
    unsafe impl<T: Sync + Send> Sync for Once<T> {}
    impl<T> Once<T> {
        pub const fn new() -> Self {
            Once { set: AtomicBool::new(false), v: UnsafeCell::new(None) }
        }
        /// logOS is single-threaded; this is only ever called from the main loop.
        pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
            if !self.set.load(Ordering::Acquire) {
                unsafe { *self.v.get() = Some(f()) };
                self.set.store(true, Ordering::Release);
            }
            unsafe { (*self.v.get()).as_ref().unwrap() }
        }
    }
}

fn config() -> Result<Arc<ClientConfig>, String> {
    let mut err = None;
    let c = CONFIG.get_or_init(|| {
        let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let built = ClientConfig::builder_with_details(Arc::new(rustls_rustcrypto::provider()), Arc::new(RtcClock))
            .with_safe_default_protocol_versions()
            .map(|b| b.with_root_certificates(roots).with_no_client_auth());
        match built {
            Ok(c) => Arc::new(c),
            Err(e) => {
                err = Some(format!("{}", e));
                // Keep a placeholder so we do not rebuild every request; the error is reported once.
                Arc::new(
                    ClientConfig::builder_with_details(Arc::new(rustls_rustcrypto::provider()), Arc::new(RtcClock))
                        .with_protocol_versions(&[&rustls::version::TLS13])
                        .expect("TLS 1.3")
                        .with_root_certificates(RootCertStore::empty())
                        .with_no_client_auth(),
                )
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(c.clone()),
    }
}

/// One HTTPS exchange: send `request` once the handshake is done, collect the plaintext reply.
pub struct Tls {
    conn: UnbufferedClientConnection,
    inc: Vec<u8>,
    used: usize,
    request: Option<Vec<u8>>,
    pub plain: Vec<u8>,
    /// The peer closed the session (close_notify), or the session failed.
    pub done: bool,
    pub err: Option<String>,
    pub handshaken: bool,
}

impl Tls {
    pub fn new(host: &'static str, request: Vec<u8>) -> Result<Tls, String> {
        let name = ServerName::try_from(host).map_err(|e| format!("bad host name: {}", e))?;
        let conn = UnbufferedClientConnection::new(config()?, name).map_err(|e| format!("{}", e))?;
        Ok(Tls { conn, inc: vec![0; 32 * 1024], used: 0, request: Some(request), plain: Vec::new(), done: false, err: None, handshaken: false })
    }

    /// Ciphertext that came in from the socket.
    pub fn feed(&mut self, data: &[u8]) {
        if self.used + data.len() > self.inc.len() {
            self.inc.resize(self.used + data.len() + 16 * 1024, 0);
        }
        self.inc[self.used..self.used + data.len()].copy_from_slice(data);
        self.used += data.len();
    }

    /// Drive the state machine as far as it goes without more input.
    /// Returns ciphertext to put on the wire.
    pub fn pump(&mut self) -> Vec<u8> {
        let mut tx = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        for _ in 0..256 {
            if self.done {
                break;
            }
            let UnbufferedStatus { mut discard, state } = self.conn.process_tls_records(&mut self.inc[..self.used]);
            let mut stop = false;
            match state {
                Err(e) => {
                    self.err = Some(format!("{}", e));
                    self.done = true;
                }
                Ok(ConnectionState::ReadTraffic(mut st)) => {
                    while let Some(rec) = st.next_record() {
                        match rec {
                            Ok(r) => {
                                discard += r.discard;
                                self.plain.extend_from_slice(r.payload);
                            }
                            Err(e) => {
                                self.err = Some(format!("{}", e));
                                self.done = true;
                                break;
                            }
                        }
                    }
                }
                Ok(ConnectionState::EncodeTlsData(mut st)) => loop {
                    match st.encode(&mut buf) {
                        Ok(n) => {
                            tx.extend_from_slice(&buf[..n]);
                            break;
                        }
                        Err(EncodeError::InsufficientSize(s)) => buf.resize(s.required_size, 0),
                        Err(e) => {
                            self.err = Some(format!("{:?}", e));
                            self.done = true;
                            break;
                        }
                    }
                },
                // We return `tx` to the caller, who sends it right away.
                Ok(ConnectionState::TransmitTlsData(st)) => st.done(),
                Ok(ConnectionState::BlockedHandshake) => stop = true,
                Ok(ConnectionState::WriteTraffic(mut st)) => {
                    if !self.handshaken {
                        self.handshaken = true;
                        log::info!("TLS handshake done");
                    }
                    if let Some(req) = self.request.take() {
                        loop {
                            match st.encrypt(&req, &mut buf) {
                                Ok(n) => {
                                    tx.extend_from_slice(&buf[..n]);
                                    break;
                                }
                                Err(EncryptError::InsufficientSize(s)) => buf.resize(s.required_size, 0),
                                Err(e) => {
                                    self.err = Some(format!("{:?}", e));
                                    self.done = true;
                                    break;
                                }
                            }
                        }
                    }
                    stop = true;
                }
                Ok(ConnectionState::PeerClosed) | Ok(ConnectionState::Closed) => self.done = true,
                Ok(_) => stop = true,
            }
            if discard > 0 {
                self.inc.copy_within(discard..self.used, 0);
                self.used -= discard;
            }
            if stop {
                break;
            }
        }
        tx
    }
}
