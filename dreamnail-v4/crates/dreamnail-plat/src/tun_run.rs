//! The actual tun2proxy run loop, factored out so it can run in a SEPARATE,
//! HEADLESS, privileged helper process (`dreamnail-tunhelper`) instead of inside
//! the GUI. Creating the TUN device + editing the routing table needs root /
//! Administrator; running that in the GUI meant elevating the whole app, which is
//! fragile on every OS (Linux: the GTK image sandbox can't run as root; macOS:
//! the desktop won't draw a root window; Windows: UAC may pick another account).
//! A headless helper has no GUI, so none of that applies — it just needs the
//! privilege for the network work.
//!
//! [`TunProxy`](crate::tun::TunProxy) (in the GUI) SPAWNS this via the platform's
//! elevation prompt; the helper calls [`run_blocking`] and stays up until told to
//! stop (see the helper binary). On stop, tun2proxy's `TproxyState` `Drop`
//! restores the original routing table + DNS.

use std::net::SocketAddr;
use std::str::FromStr;

use dreamnail_core::ports::{EngineError, Result};
use tproxy_config::IpCidr;
use tun2proxy::{ArgDns, ArgProxy, ArgVerbosity, Args, CancellationToken, ProxyType};

/// Everything the TUN worker needs, passed from the GUI to the helper as CLI
/// flags. Kept small + plain so it round-trips cleanly across the process (and
/// privilege) boundary.
#[derive(Debug, Clone)]
pub struct TunRunConfig {
    /// Local SOCKS5 the captured traffic is forwarded to (the app's Xray inbound).
    pub proxy_addr: SocketAddr,
    /// Server IP to BYPASS so the SSH socket to it isn't captured (avoids a loop).
    pub bypass_ip: String,
    pub mtu: u16,
    pub mss: u16,
    /// Local address of the udpgw forward (UDP-over-TCP gateway). When set,
    /// tun2proxy sends ALL UDP through it instead of SOCKS UDP-associate — the
    /// fix for macOS, where UDP-associate dies with EISCONN. `None` keeps the old
    /// SOCKS UDP-associate path.
    pub udpgw_addr: Option<SocketAddr>,
}

impl TunRunConfig {
    /// Build from string parts (the helper's parsed CLI args). `udpgw` is the
    /// optional local udpgw address ("127.0.0.1:10730"); empty/None disables it.
    pub fn parse(
        proxy: &str,
        bypass_ip: &str,
        mtu: u16,
        mss: u16,
        udpgw: Option<&str>,
    ) -> Result<Self> {
        let proxy_addr = SocketAddr::from_str(proxy)
            .map_err(|e| EngineError::Proxy(format!("bad SOCKS address {proxy}: {e}")))?;
        let udpgw_addr = match udpgw {
            Some(s) if !s.is_empty() => Some(
                SocketAddr::from_str(s)
                    .map_err(|e| EngineError::Proxy(format!("bad udpgw address {s}: {e}")))?,
            ),
            _ => None,
        };
        Ok(Self {
            proxy_addr,
            bypass_ip: bypass_ip.to_string(),
            mtu,
            mss,
            udpgw_addr,
        })
    }
}

/// Run tun2proxy on the CURRENT process (must already be privileged). Blocks
/// until `cancel` is triggered or the worker exits on its own, then returns —
/// routing/DNS are restored as the run unwinds. Meant to be the whole body of the
/// helper binary; never call this from the GUI process (that's the bug we fixed).
pub fn run_blocking(cfg: TunRunConfig, cancel: CancellationToken) -> Result<()> {
    let proxy = ArgProxy {
        proxy_type: ProxyType::Socks5,
        addr: cfg.proxy_addr,
        credentials: None,
    };
    let bypass = IpCidr::from_str(&format!("{}/32", cfg.bypass_ip))
        .map_err(|e| EngineError::Proxy(format!("bad bypass CIDR: {e}")))?;

    let mut args = Args::default();
    args.proxy(proxy).bypass(bypass).setup(true);
    args.mtu = cfg.mtu;
    args.tcp_mss = Some(cfg.mss);
    // DNS handling. The DEFAULT (`ArgDns::Direct`) relays DNS as raw UDP through
    // the SOCKS proxy (UDP ASSOCIATE). On macOS that path is broken — every DNS
    // query dies with EISCONN ("Socket is already connected", os error 56), so no
    // hostname resolves and no site loads even though TCP routes fine. `Virtual`
    // (Fake-IP) makes tun2proxy answer DNS itself and resolve names over TCP
    // through the proxy, so no UDP is involved at all. This is the exact fix for
    // the macOS "connects but websites won't load" symptom; it's also correct on
    // Linux/Windows, so we use it everywhere for one code path.
    args.dns(ArgDns::Virtual);
    // UDP gateway: when configured, tun2proxy tunnels UDP over a TCP connection to
    // the server-side udpgw-server (reached via the udpgw SSH forward) instead of
    // using SOCKS UDP-associate. This is the fix for macOS game/VoIP UDP — the
    // UDP-associate path there fails with EISCONN — and works identically on Linux
    // and Windows, so we use it on every OS when set. DNS still goes over Virtual
    // (Fake-IP over TCP); everything else UDP now has a real path out.
    if let Some(udpgw) = cfg.udpgw_addr {
        args.udpgw_server(udpgw);
    }
    // Info keeps the useful lines (setup done, errors) without the per-packet
    // spam that Trace produces. The helper installs the logger that carries these
    // to /tmp/dreamnail-tunhelper.log; set DREAMNAIL_TUN_LOG=trace to go deep.
    args.verbosity = ArgVerbosity::Info;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| EngineError::Proxy(format!("tokio runtime: {e}")))?;
    // `packet_information`: macOS utun devices ALWAYS prepend a 4-byte
    // address-family header to every packet; Linux/Windows TUN do not. If this is
    // wrong, tun2proxy reads every packet shifted by 4 bytes and drops them ALL as
    // "unknown transport" — the exact macOS-only failure that made stealth freeze
    // then fall back while Linux worked. Mirror tun2proxy's own binary, which
    // passes `cfg!(target_os = "macos")` here.
    let packet_information = cfg!(target_os = "macos");
    rt.block_on(tun2proxy::general_run_async(
        args,
        cfg.mtu,
        packet_information,
        cancel,
    ))
    .map(|_| ())
    .map_err(|e| EngineError::Proxy(e.to_string()))
}

/// Re-export so the helper binary can build a token without depending on
/// tun2proxy directly.
pub use tun2proxy::CancellationToken as TunCancel;
