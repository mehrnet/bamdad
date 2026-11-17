//! Privileged half of Bamdad's desktop tunnel.
//!
//! Creating a TUN device and rewriting the default route needs root; the app
//! runs as the user. This helper does exactly that and nothing else: it opens
//! the device, installs the routes the tunnel needs, keeps the descriptor open
//! (closing it destroys the interface), and restores the original routing when
//! it is asked to stop.
//!
//! The app starts it through `pkexec` (or sudo) and, when a payload is given,
//! the helper hands the tun descriptor to that child as fd 3.
//!
//! Example:
//!   bamdad-tun-helper up --name bamdad0 --address 10.255.0.1/24 --mtu 1400 \
//!       --bypass 203.0.113.7/32 --state /run/bamdad-tun.bamdad0 \
//!       --exec /usr/bin/bamdad-tun2socks
use std::fs;
use std::io::Write;
use std::net::Ipv4Addr;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

const DEV_NET_TUN: &str = "/dev/net/tun";
const TUNSETIFF: libc::c_ulong = 0x4004_54ca;

pub fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("bamdad-tun-helper: {error}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let options = Options::parse(args.collect())?;

    match command.as_str() {
        "up" => up(options),
        "down" => down(options),
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown command '{other}'\n{}", usage())),
    }
}

fn usage() -> String {
    "usage: bamdad-tun-helper up --name IFACE --address ADDR/PREFIX [--mtu N] \\
        [--address6 ADDR/PREFIX] [--no-ipv6] [--bypass CIDR]... [--dns ADDR]... \
        [--dns-method auto|resolved|file] \
        [--resolv-conf FILE] \
        [--state FILE] [--parent-pid PID] [--exec PROGRAM [ARGS...]]
   bamdad-tun-helper down --state FILE

'up' configures a TUN device, routes the default through it, keeps the
descriptor open until SIGINT/SIGTERM, then restores the original routing."
        .to_string()
}

#[derive(Debug, Default)]
struct Options {
    name: String,
    address: String,
    address6: Option<String>,
    mtu: u16,
    bypass: Vec<String>,
    dns: Vec<String>,
    dns_method: String,
    resolv: PathBuf,
    state: Option<PathBuf>,
    parent_pid: Option<i32>,
    exec: Vec<String>,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Options {
            mtu: 1400,
            address6: Some("fd00::1/64".to_string()),
            dns_method: "auto".to_string(),
            resolv: PathBuf::from("/etc/resolv.conf"),
            ..Default::default()
        };
        let mut rest = args.into_iter();
        while let Some(arg) = rest.next() {
            let value = |rest: &mut std::vec::IntoIter<String>| {
                rest.next()
                    .ok_or_else(|| format!("missing value for {arg}"))
            };
            match arg.as_str() {
                "--name" => options.name = value(&mut rest)?,
                "--address" => options.address = value(&mut rest)?,
                "--address6" => options.address6 = Some(value(&mut rest)?),
                "--no-ipv6" => options.address6 = None,
                "--mtu" => options.mtu = value(&mut rest)?.parse().map_err(|_| "bad --mtu")?,
                "--bypass" => options.bypass.push(value(&mut rest)?),
                "--dns" => options.dns.push(value(&mut rest)?),
                "--dns-method" => options.dns_method = value(&mut rest)?,
                "--resolv-conf" => options.resolv = PathBuf::from(value(&mut rest)?),
                "--state" => options.state = Some(PathBuf::from(value(&mut rest)?)),
                "--parent-pid" => {
                    options.parent_pid =
                        Some(value(&mut rest)?.parse().map_err(|_| "bad --parent-pid")?)
                }
                "--exec" => {
                    options.exec = rest.collect();
                    break;
                }
                other => return Err(format!("unknown option '{other}'")),
            }
        }
        if options.name.is_empty() || options.address.is_empty() {
            return Err("--name and --address are required".to_string());
        }
        if !options.address.contains('/') {
            return Err("--address needs a prefix, e.g. 10.255.0.1/24".to_string());
        }
        Ok(options)
    }
}

fn up(options: Options) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("must run as root".to_string());
    }

    let default = ip_output(&["route", "show", "default"]).ok();
    let default6 = ip_output(&["-6", "route", "show", "default"]).ok();
    let snapshot_default = default
        .as_deref()
        .and_then(|text| text.lines().next())
        .map(|line| line.trim().to_string());

    let snapshot_default6 = default6
        .as_deref()
        .and_then(|text| text.lines().next())
        .map(|line| line.trim().to_string());

    let fd = open_tun(&options.name)?;
    run_ip(&["addr", "add", &options.address, "dev", &options.name])?;
    run_ip(&[
        "link",
        "set",
        "dev",
        &options.name,
        "mtu",
        &options.mtu.to_string(),
        "up",
    ])?;

    // Give the tunnel an IPv6 address and send the v6 default into it, so hosts
    // with IPv6 connectivity cannot bypass the tunnel. Needs the link up first.
    if let Some(address6) = &options.address6 {
        if run_ip(&["-6", "addr", "add", address6, "dev", &options.name]).is_ok() {
            run_ip(&["-6", "route", "replace", "default", "dev", &options.name])?;
        } else {
            eprintln!("bamdad-tun-helper: ignoring IPv6 address {address6}");
        }
    }

    let gateway = snapshot_default
        .as_deref()
        .and_then(|line| parse_default(line));
    let mut added = Vec::new();
    if let Some((via, dev)) = &gateway {
        for bypass in &options.bypass {
            run_ip(&["route", "replace", bypass, "via", via, "dev", dev])?;
            added.push(bypass.clone());
        }
    }

    // Prefer the tunnel for everything else. `replace` keeps the old route in
    // the snapshot only; it is restored on the way down.
    run_ip(&["route", "replace", "default", "dev", &options.name])?;

    let dns = apply_dns(
        &options.name,
        &options.dns,
        &options.dns_method,
        &options.resolv,
    );
    match &dns {
        DnsMethod::Resolved => println!("dns: systemd-resolved"),
        DnsMethod::File { .. } => println!("dns: {} (backup kept)", options.resolv.display()),
        DnsMethod::None => println!("dns: not configured"),
    }

    if let Some(path) = &options.state {
        write_state(
            path,
            snapshot_default.as_deref(),
            snapshot_default6.as_deref(),
            &added,
            &dns,
            &options.resolv,
            &options.name,
        )?;
    }

    println!("ready {}", options.name);
    let _ = std::io::stdout().flush();

    install_signal_handlers();

    let mut child = if options.exec.is_empty() {
        None
    } else {
        Some(spawn_payload(&options.exec, fd)?)
    };

    while !STOP.load(Ordering::SeqCst) {
        if let Some(child) = child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
        }
        // If the app that started us disappeared, tear the tunnel down instead
        // of leaving the machine with routes into a dead device.
        if let Some(pid) = options.parent_pid {
            if unsafe { libc::kill(pid, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                eprintln!("bamdad-tun-helper: parent {pid} is gone, stopping");
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }

    restore_dns(&options.name, &dns, &options.resolv);
    restore(
        snapshot_default.as_deref(),
        snapshot_default6.as_deref(),
        &added,
    );
    if let Some(path) = &options.state {
        let _ = fs::remove_file(path);
    }
    unsafe { libc::close(fd) };
    println!("stopped");
    Ok(())
}

fn down(options: Options) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("must run as root".to_string());
    }
    let path = options
        .state
        .ok_or_else(|| "down needs --state FILE".to_string())?;
    let text = fs::read_to_string(&path).map_err(|error| format!("{error}"))?;
    let mut default = None;
    let mut default6 = None;
    let mut added = Vec::new();
    let mut dns = DnsMethod::None;
    let mut resolv = PathBuf::from("/etc/resolv.conf");
    let mut iface = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("default=") {
            default = Some(value.to_string()).filter(|value| !value.is_empty());
        } else if let Some(value) = line.strip_prefix("default6=") {
            default6 = Some(value.to_string()).filter(|value| !value.is_empty());
        } else if let Some(value) = line.strip_prefix("bypass=") {
            added.push(value.to_string());
        } else if let Some(value) = line.strip_prefix("dns=") {
            dns = match value {
                "resolved" => DnsMethod::Resolved,
                "file" => DnsMethod::File {
                    backup: PathBuf::from(format!("{}.bamdad-backup", resolv.display())),
                },
                _ => DnsMethod::None,
            };
        } else if let Some(value) = line.strip_prefix("resolv=") {
            resolv = PathBuf::from(value);
        } else if let Some(value) = line.strip_prefix("iface=") {
            iface = value.to_string();
        }
    }
    if !iface.is_empty() {
        restore_dns(&iface, &dns, &resolv);
    }
    restore(default.as_deref(), default6.as_deref(), &added);
    let _ = fs::remove_file(&path);
    println!("stopped");
    Ok(())
}

fn restore(default: Option<&str>, default6: Option<&str>, added: &[String]) {
    if let Some(line) = default {
        if let Some((via, dev)) = parse_default(line) {
            let _ = run_ip(&["route", "replace", "default", "via", &via, "dev", &dev]);
        }
    } else {
        // No default route existed before: remove the one we added.
        let _ = run_ip(&["route", "del", "default"]);
    }
    match default6 {
        Some(line) => {
            if let Some((via, dev)) = parse_default(line) {
                let _ = run_ip(&[
                    "-6", "route", "replace", "default", "via", &via, "dev", &dev,
                ]);
            }
        }
        None => {
            let _ = run_ip(&["-6", "route", "del", "default"]);
        }
    }
    for cidr in added {
        let _ = run_ip(&["route", "del", cidr]);
    }
}

/// How DNS was pointed at the tunnel.
enum DnsMethod {
    /// systemd-resolved: per-link servers plus a routing domain.
    Resolved,
    /// Plain `/etc/resolv.conf` rewrite with the original kept next to it.
    File { backup: PathBuf },
    /// No DNS configured (empty server list, or both paths failed).
    None,
}

fn resolvectl_binary() -> Option<&'static str> {
    ["/usr/bin/resolvectl", "/bin/resolvectl"]
        .into_iter()
        .find(|candidate| std::path::Path::new(candidate).exists())
}

fn apply_dns(iface: &str, servers: &[String], method: &str, resolv: &std::path::Path) -> DnsMethod {
    if servers.is_empty() {
        return DnsMethod::None;
    }

    if method != "file" {
        if let Some(method) = apply_dns_resolved(iface, servers) {
            return method;
        }
        if method == "resolved" {
            eprintln!("bamdad-tun-helper: systemd-resolved is not usable here");
            return DnsMethod::None;
        }
    }

    apply_dns_file(servers, resolv)
}

fn apply_dns_resolved(iface: &str, servers: &[String]) -> Option<DnsMethod> {
    if let Some(resolvectl) = resolvectl_binary() {
        let ok = Command::new(resolvectl)
            .arg("dns")
            .arg(iface)
            .args(servers)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        let routed = Command::new(resolvectl)
            .args(["domain", iface, "~."])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if ok && routed {
            return Some(DnsMethod::Resolved);
        }
    }
    None
}

fn apply_dns_file(servers: &[String], resolv: &std::path::Path) -> DnsMethod {
    let backup = PathBuf::from(format!("{}.bamdad-backup", resolv.display()));
    if let Ok(existing) = fs::read(resolv) {
        let _ = fs::write(&backup, existing);
    }
    let body = format!(
        "# generated by bamdad-tun-helper\n{}\n",
        servers
            .iter()
            .map(|server| format!("nameserver {server}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    match fs::write(resolv, body) {
        Ok(()) => DnsMethod::File { backup },
        Err(error) => {
            eprintln!(
                "bamdad-tun-helper: could not update {}: {error}",
                resolv.display()
            );
            DnsMethod::None
        }
    }
}

fn restore_dns(iface: &str, method: &DnsMethod, resolv: &std::path::Path) {
    match method {
        DnsMethod::Resolved => {
            if let Some(resolvectl) = resolvectl_binary() {
                let _ = Command::new(resolvectl).args(["revert", iface]).output();
            }
        }
        DnsMethod::File { backup } => {
            let _ = fs::rename(backup, resolv);
        }
        DnsMethod::None => {}
    }
}

fn parse_default(line: &str) -> Option<(String, String)> {
    let mut via = None;
    let mut dev = None;
    let mut parts = line.split_whitespace();
    while let Some(part) = parts.next() {
        match part {
            "via" => via = parts.next().map(str::to_string),
            "dev" => dev = parts.next().map(str::to_string),
            _ => {}
        }
    }
    Some((via?, dev?))
}

fn write_state(
    path: &PathBuf,
    default: Option<&str>,
    default6: Option<&str>,
    added: &[String],
    dns: &DnsMethod,
    resolv: &std::path::Path,
    iface: &str,
) -> Result<(), String> {
    let mut text = String::new();
    text.push_str(&format!("default={}\n", default.unwrap_or("")));
    text.push_str(&format!("default6={}\n", default6.unwrap_or("")));
    for cidr in added {
        text.push_str(&format!("bypass={cidr}\n"));
    }
    text.push_str(&format!("iface={iface}\n"));
    text.push_str(&format!("resolv={}\n", resolv.display()));
    text.push_str(&format!(
        "dns={}\n",
        match dns {
            DnsMethod::Resolved => "resolved",
            DnsMethod::File { .. } => "file",
            DnsMethod::None => "none",
        }
    ));
    fs::write(path, text).map_err(|error| format!("state file: {error}"))
}

fn open_tun(name: &str) -> Result<i32, String> {
    let cpath = std::ffi::CString::new(DEV_NET_TUN).map_err(|error| error.to_string())?;
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "open {DEV_NET_TUN}: {} (is /dev/net/tun available?)",
            std::io::Error::last_os_error()
        ));
    }

    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    let bytes = name.as_bytes();
    if bytes.len() >= libc::IFNAMSIZ {
        unsafe { libc::close(fd) };
        return Err(format!("interface name too long: {name}"));
    }
    for (slot, byte) in request.ifr_name.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    request.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
    if unsafe { libc::ioctl(fd, TUNSETIFF, &mut request) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("TUNSETIFF {name}: {error}"));
    }
    Ok(fd)
}

fn spawn_payload(argv: &[String], tun_fd: i32) -> Result<std::process::Child, String> {
    let mut command = Command::new(&argv[0]);
    unsafe {
        command
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env("BAMDAD_TUN_FD", "3")
            // Move the tun descriptor to fd 3. CLOEXEC is cleared by the dup.
            .pre_exec(move || {
                if libc::dup2(tun_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
    }
    command
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", argv[0]))
}

fn install_signal_handlers() {
    extern "C" fn handler(_: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }
    let handler = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

fn ip_binary() -> &'static str {
    for candidate in ["/usr/sbin/ip", "/sbin/ip", "/usr/bin/ip", "/bin/ip"] {
        if std::path::Path::new(candidate).exists() {
            return candidate;
        }
    }
    "ip"
}

fn run_ip(args: &[&str]) -> Result<(), String> {
    let output = Command::new(ip_binary())
        .args(args)
        .output()
        .map_err(|error| format!("ip {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "ip {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn ip_output(args: &[&str]) -> Result<String, String> {
    let output = Command::new(ip_binary())
        .args(args)
        .output()
        .map_err(|error| format!("ip {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[allow(dead_code)]
fn parse_cidr(value: &str) -> Option<(Ipv4Addr, u8)> {
    let (addr, prefix) = value.split_once('/')?;
    Some((addr.parse().ok()?, prefix.parse().ok()?))
}
