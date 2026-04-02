//! `savfox dns` -- DNS-SD/CoreDNS/Tailscale bootstrap helpers.

use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use clap::Parser;
use savfox_core::config::find_savfox_home;

#[derive(Debug, Parser)]
pub struct DnsCommand {
    #[clap(subcommand)]
    pub action: DnsAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum DnsAction {
    /// Generate DNS-SD/CoreDNS configuration and optional refresh watcher.
    Setup {
        /// DNS zone domain (without trailing dot).
        #[clap(long, default_value = "savfox.internal")]
        domain: String,
        /// Hostname for the gateway A record.
        #[clap(long, default_value = "gateway")]
        host: String,
        /// Service port for _savfox._tcp SRV.
        #[clap(long, default_value_t = 18881)]
        port: u16,
        /// Explicit IP address (auto-detected if omitted).
        #[clap(long)]
        ip: Option<IpAddr>,
        /// Output directory (defaults to {savfox_home}/dns).
        #[clap(long)]
        out_dir: Option<PathBuf>,
        /// Keep running and refresh zone files when IP changes.
        #[clap(long, default_value_t = false)]
        watch: bool,
        /// Poll interval (seconds) for --watch mode.
        #[clap(long, default_value_t = 15)]
        interval_secs: u64,
    },
}

pub async fn run(cmd: DnsCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd.action {
        DnsAction::Setup {
            domain,
            host,
            port,
            ip,
            out_dir,
            watch,
            interval_secs,
        } => {
            let out_dir = if let Some(path) = out_dir {
                path
            } else {
                find_savfox_home()?.join("dns")
            };
            std::fs::create_dir_all(&out_dir)?;

            let mut current_ip = ip
                .or_else(detect_primary_ip)
                .ok_or_else(|| "failed to detect local IP; pass --ip explicitly".to_owned())?;

            write_dns_bundle(&out_dir, &domain, &host, current_ip, port)?;
            print_dns_summary(&out_dir, &domain, &host, current_ip, port);

            if watch {
                println!(
                    "Watching for IP changes every {interval_secs}s (Ctrl+C to stop)..."
                );
                let poll = Duration::from_secs(interval_secs.max(2));
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            println!("Stopped DNS watcher.");
                            break;
                        }
                        _ = tokio::time::sleep(poll) => {
                            if let Some(latest_ip) = detect_primary_ip()
                                && latest_ip != current_ip
                            {
                                current_ip = latest_ip;
                                write_dns_bundle(&out_dir, &domain, &host, current_ip, port)?;
                                println!("IP changed, refreshed zone files: {current_ip}");
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn write_dns_bundle(
    out_dir: &std::path::Path,
    domain: &str,
    host: &str,
    ip: IpAddr,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let zone_file_path = out_dir.join(format!("{domain}.zone"));
    let corefile_path = out_dir.join("Corefile.savfox");
    let tailscale_doc_path = out_dir.join("tailscale-split-dns.md");

    std::fs::write(&zone_file_path, render_zone_file(domain, host, ip, port))?;
    std::fs::write(&corefile_path, render_coredns_snippet(domain))?;
    std::fs::write(
        &tailscale_doc_path,
        render_tailscale_doc(domain, zone_file_path.as_path()),
    )?;
    Ok(())
}

fn render_zone_file(domain: &str, host: &str, ip: IpAddr, port: u16) -> String {
    let serial = chrono_like_serial();
    format!(
        "$ORIGIN {domain}.\n\
@ 3600 IN SOA ns1.{domain}. admin.{domain}. (\n\
  {serial} ; serial\n\
  7200 ; refresh\n\
  3600 ; retry\n\
  1209600 ; expire\n\
  3600 ; minimum\n\
)\n\
@     3600 IN NS ns1.{domain}.\n\
ns1   3600 IN A  {ip}\n\
{host} 3600 IN A  {ip}\n\
_savfox._tcp 3600 IN SRV 0 5 {port} {host}.{domain}.\n\
"
    )
}

fn render_coredns_snippet(domain: &str) -> String {
    format!(
        "{domain}:53 {{\n\
    file /etc/coredns/{domain}.zone {domain}\n\
    log\n\
    errors\n\
}}\n"
    )
}

fn render_tailscale_doc(domain: &str, zone_file_path: &std::path::Path) -> String {
    format!(
        "# Tailscale Split DNS for Savfox\n\n\
Domain: `{domain}`\n\n\
## 1) Deploy CoreDNS\n\
- Copy `{domain}.zone` to your CoreDNS host (for example `/etc/coredns/{domain}.zone`).\n\
- Merge `Corefile.savfox` into your CoreDNS Corefile.\n\
- Restart CoreDNS.\n\n\
## 2) Configure Tailscale admin DNS\n\
- In Tailscale Admin Console -> DNS -> Split DNS:\n\
  - Domain: `{domain}`\n\
  - Nameserver: your CoreDNS node IP\n\n\
## 3) Optional CLI\n\
- On Linux/macOS clients:\n\
  - `tailscale up --accept-dns=true`\n\n\
Generated from: `{}`\n",
        zone_file_path.display()
    )
}

fn print_dns_summary(out_dir: &std::path::Path, domain: &str, host: &str, ip: IpAddr, port: u16) {
    println!("DNS setup complete.");
    println!("Domain: {domain}");
    println!("Gateway host: {host}.{domain} -> {ip}");
    println!("Service: _savfox._tcp.{domain} -> {host}.{domain}:{port}");
    println!("Wrote:");
    println!("  - {}", out_dir.join(format!("{domain}.zone")).display());
    println!("  - {}", out_dir.join("Corefile.savfox").display());
    println!("  - {}", out_dir.join("tailscale-split-dns.md").display());
}

fn detect_primary_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("8.8.8.8", 80)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_unspecified() { None } else { Some(ip) }
}

fn chrono_like_serial() -> u64 {
    // yyyymmddNN style-ish serial without introducing extra deps.
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let day = now / 86_400;
    let seq = now % 100;
    day * 100 + seq
}
