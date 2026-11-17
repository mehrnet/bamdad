//! macOS half of the privileged desktop helper: utun device plus routing.
//!
//! utun devices do not require root to create, but assigning addresses and
//! changing routes do, so this runs as the privileged helper like the Linux
//! one. Unlike Linux, the kernel picks the interface name (`utunN`), which is
//! reported on the `ready` line and stored in the state file.
//!
//! Routing supports two modes:
//!   --route CIDR   send just that prefix into the tunnel (safe for tests and
//!                  split tunnelling)
//!   --default      send all IPv4 traffic into the tunnel using the /1 split
//!                  and keep the profile endpoint on the physical link
//!
//! DNS is not rewritten yet; that needs `scutil`/`networksetup` handling per
//! service and is left for when the sidecar itself is validated on macOS.

use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

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
    "usage: bamdad-tun-helper up [--name utunN] --address ADDR [--mtu N] \\
        (--route CIDR | --default --bypass IP)... [--dns ADDR]... \
        [--dns-method auto|networksetup|none] [--state FILE] [--parent-pid PID] \\
        [--exec PROGRAM [ARGS...]]
   bamdad-tun-helper down --state FILE"
        .to_string()
}

#[derive(Debug, Default)]
struct Options {
    name: Option<String>,
    address: String,
    mtu: u16,
    routes: Vec<String>,
    default: bool,
    bypass: Vec<String>,
    dns: Vec<String>,
    dns_method: String,
    state: Option<PathBuf>,
    parent_pid: Option<i32>,
    exec: Vec<String>,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Options {
            mtu: 1400,
            dns_method: "auto".to_string(),
            ..Default::default()
        };
        let mut rest = args.into_iter();
        while let Some(arg) = rest.next() {
            let mut value = || {
                rest.next()
                    .ok_or_else(|| format!("missing value for {arg}"))
            };
            match arg.as_str() {
                "--name" => options.name = Some(value()?),
                "--address" => options.address = value()?,
                "--mtu" => options.mtu = value()?.parse().map_err(|_| "bad --mtu")?,
                "--route" => options.routes.push(value()?),
                "--default" => options.default = true,
                "--bypass" => options.bypass.push(value()?),
                "--dns" => options.dns.push(value()?),
                "--dns-method" => options.dns_method = value()?,
                "--state" => options.state = Some(PathBuf::from(value()?)),
                "--parent-pid" => {
                    options.parent_pid = Some(value()?.parse().map_err(|_| "bad --parent-pid")?)
                }
                "--exec" => {
                    options.exec = rest.collect();
                    break;
                }
                other => return Err(format!("unknown option '{other}'")),
            }
        }
        if options.address.is_empty() {
            return Err("--address is required".to_string());
        }
        if !options.default && options.routes.is_empty() {
            return Err("give at least one --route or --default".to_string());
        }
        Ok(options)
    }
}

fn up(options: Options) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("must run as root".to_string());
    }

    let (fd, device) = open_utun(options.name.as_deref())?;
    let mut added: Vec<String> = Vec::new();

    let address = options
        .address
        .split('/')
        .next()
        .unwrap_or(&options.address);
    exec_command(
        "ifconfig",
        &[
            &device,
            "inet",
            address,
            address,
            "netmask",
            "255.255.255.0",
            "up",
        ],
    )?;
    exec_command("ifconfig", &[&device, "mtu", &options.mtu.to_string()])?;

    for route in &options.routes {
        if add_route(route, &device).is_ok() {
            added.push(route.clone());
        }
    }

    if options.default {
        // Split default: two /1 routes cover IPv4 without touching the real
        // default, which makes restoring trivial.
        for half in ["0.0.0.0/1", "128.0.0.0/1"] {
            add_route(half, &device)?;
            added.push(half.to_string());
        }
        if let Some(gateway) = default_gateway() {
            for bypass in &options.bypass {
                let host = bypass.split('/').next().unwrap_or(bypass);
                if exec_command("route", &["-n", "add", "-host", host, &gateway]).is_ok() {
                    added.push(format!("host:{host}"));
                }
            }
        }
    }

    let dns_services = apply_dns(&options.dns, &options.dns_method);

    if let Some(path) = &options.state {
        // The pid lets a privileged `kill` stop us after a crash; the app
        // cannot signal a root process on its own.
        let mut text = format!(
            "device={device}\npid={}\n",
            unsafe { libc::getpid() }
        );
        for entry in &added {
            text.push_str(&format!("route={entry}\n"));
        }
        for service in &dns_services {
            text.push_str(&format!("dns={service}\n"));
        }
        fs::write(path, text).map_err(|error| format!("state file: {error}"))?;
    }

    println!("ready {device}");
    let _ = std::io::stdout().flush();

    install_signal_handlers();

    // utun frames carry a 4-byte address-family header that tun2socks does not
    // expect, so packets go through a socketpair that strips and re-adds it.
    let payload_fd = if options.exec.is_empty() {
        None
    } else {
        Some(start_header_adapter(fd)?)
    };

    let mut child = match payload_fd {
        Some(payload_fd) => Some(spawn_payload(&options.exec, payload_fd)?),
        None => None,
    };

    while !STOP.load(Ordering::SeqCst) {
        if let Some(child) = child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
        }
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

    for service in &dns_services {
        let _ = exec_command("networksetup", &["-setdnsservers", service, "Empty"]);
    }
    for entry in &added {
        let _ = remove_route(entry);
    }
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
    for line in text.lines() {
        if let Some(route) = line.strip_prefix("route=") {
            let _ = remove_route(route);
        } else if let Some(service) = line.strip_prefix("dns=") {
            let _ = exec_command("networksetup", &["-setdnsservers", service, "Empty"]);
        }
    }
    let _ = fs::remove_file(&path);
    println!("stopped");
    Ok(())
}

/// Points every network service at the tunnel's DNS servers. Returns the
/// services that were changed so they can be reset on the way down.
fn apply_dns(servers: &[String], method: &str) -> Vec<String> {
    if servers.is_empty() || method == "none" {
        return Vec::new();
    }
    let Ok(output) = exec_command("networksetup", &["-listallnetworkservices"]) else {
        return Vec::new();
    };
    let mut changed = Vec::new();
    for line in output.lines().skip(1) {
        let service = line.trim();
        // Disabled services are prefixed with an asterisk.
        if service.is_empty() || service.starts_with('*') {
            continue;
        }
        let mut args = vec!["-setdnsservers", service];
        args.extend(servers.iter().map(String::as_str));
        if exec_command("networksetup", &args).is_ok() {
            changed.push(service.to_string());
        }
    }
    if changed.is_empty() {
        eprintln!("bamdad-tun-helper: no network service accepted the DNS servers");
    }
    changed
}

fn add_route(cidr: &str, device: &str) -> Result<(), String> {
    exec_command("route", &["-n", "add", "-net", cidr, "-interface", device]).map(|_| ())
}

fn remove_route(entry: &str) -> Result<(), String> {
    if let Some(host) = entry.strip_prefix("host:") {
        return exec_command("route", &["-n", "delete", "-host", host]).map(|_| ());
    }
    exec_command("route", &["-n", "delete", "-net", entry]).map(|_| ())
}

fn default_gateway() -> Option<String> {
    let output = Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| line.trim().strip_prefix("gateway:"))
        .map(|value| value.trim().to_string())
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

fn exec_command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("{program} {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ------------------------------------------------------------------- utun

const PF_SYSTEM: libc::c_int = 32;
const SYSPROTO_CONTROL: libc::c_int = 2;
const AF_SYS_CONTROL: u16 = 2;
const UTUN_CONTROL_NAME: &[u8] = b"com.apple.net.utun_control";
const UTUN_OPT_IFNAME: libc::c_int = 2;
const CTLIOCGINFO: libc::c_ulong = 0xc064_4e03;

#[repr(C)]
struct CtlInfo {
    ctl_id: u32,
    ctl_name: [u8; 96],
}

#[repr(C)]
struct SockaddrCtl {
    sc_len: u8,
    sc_family: u8,
    ss_sysaddr: u16,
    sc_id: u32,
    sc_unit: u32,
    sc_reserved: [u32; 5],
}

/// Pumps packets between the utun device and a socketpair, translating the
/// 4-byte Darwin address-family header into plain IP packets and back.
fn start_header_adapter(utun: i32) -> Result<i32, String> {
    let mut pair = [0i32; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, pair.as_mut_ptr()) } != 0 {
        return Err(format!(
            "socketpair: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ours = pair[0];
    let payload = pair[1];
    set_nonblocking(utun)?;
    set_nonblocking(ours)?;

    std::thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        while !STOP.load(Ordering::SeqCst) {
            if !readable(utun) {
                continue;
            }
            let read = unsafe { libc::read(utun, buffer.as_mut_ptr().cast(), buffer.len()) };
            if read <= 0 {
                continue;
            }
            let frame = &buffer[..read as usize];
            let Some(payload) = strip_af(frame) else { continue };
            unsafe { libc::write(ours, payload.as_ptr().cast(), payload.len()) };
        }
    });

    std::thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        while !STOP.load(Ordering::SeqCst) {
            if !readable(payload) {
                continue;
            }
            let read =
                unsafe { libc::read(payload, buffer[4..].as_mut_ptr().cast(), buffer.len() - 4) };
            if read <= 0 {
                continue;
            }
            let Some(family) = packet_family(&buffer[4..4 + read as usize]) else {
                continue;
            };
            buffer[..4].copy_from_slice(&family.to_be_bytes());
            let total = 4 + read as usize;
            unsafe { libc::write(utun, buffer.as_ptr().cast(), total) };
        }
    });

    Ok(payload)
}

fn set_nonblocking(fd: i32) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "fcntl(O_NONBLOCK): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn readable(fd: i32) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut poll, 1, 200) > 0 && (poll.revents & libc::POLLIN) != 0 }
}

/// Darwin address families carried in utun frame headers.
const AF_INET_DARWIN: u32 = 2;
const AF_INET6_DARWIN: u32 = 30;

fn packet_family(packet: &[u8]) -> Option<u32> {
    match packet.first().map(|byte| byte >> 4) {
        Some(4) => Some(AF_INET_DARWIN),
        Some(6) => Some(AF_INET6_DARWIN),
        _ => None,
    }
}

/// Removes the utun header, validating that it names a family we understand.
fn strip_af(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() <= 4 {
        return None;
    }
    let family = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]);
    if family != AF_INET_DARWIN && family != AF_INET6_DARWIN {
        return None;
    }
    Some(&frame[4..])
}

/// Opens a utun device. `hint` may name a specific unit (`utun4`); otherwise
/// the kernel picks the next free one.
fn open_utun(hint: Option<&str>) -> Result<(i32, String), String> {
    let fd = unsafe { libc::socket(PF_SYSTEM, libc::SOCK_DGRAM, SYSPROTO_CONTROL) };
    if fd < 0 {
        return Err(format!(
            "socket(PF_SYSTEM): {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut info = CtlInfo {
        ctl_id: 0,
        ctl_name: [0; 96],
    };
    info.ctl_name[..UTUN_CONTROL_NAME.len()].copy_from_slice(UTUN_CONTROL_NAME);
    if unsafe { libc::ioctl(fd, CTLIOCGINFO, &mut info) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("CTLIOCGINFO: {error}"));
    }

    let unit = hint
        .and_then(|name| name.strip_prefix("utun"))
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let address = SockaddrCtl {
        sc_len: std::mem::size_of::<SockaddrCtl>() as u8,
        sc_family: libc::AF_SYSTEM as u8,
        ss_sysaddr: AF_SYS_CONTROL,
        sc_id: info.ctl_id,
        sc_unit: unit,
        sc_reserved: [0; 5],
    };
    if unsafe {
        libc::connect(
            fd,
            &address as *const SockaddrCtl as *const libc::sockaddr,
            std::mem::size_of::<SockaddrCtl>() as libc::socklen_t,
        )
    } < 0
    {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("connect(utun): {error}"));
    }

    let mut name = [0u8; 64];
    let mut length = name.len() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            SYSPROTO_CONTROL,
            UTUN_OPT_IFNAME,
            name.as_mut_ptr() as *mut libc::c_void,
            &mut length,
        )
    } < 0
    {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("getsockopt(UTUN_OPT_IFNAME): {error}"));
    }
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    let device = String::from_utf8_lossy(&name[..end]).to_string();
    Ok((fd, device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_utun_header() {
        let mut frame = 2u32.to_be_bytes().to_vec();
        frame.extend_from_slice(&[0x45, 0, 0, 20, 0, 0]);
        assert_eq!(strip_af(&frame).unwrap(), &[0x45, 0, 0, 20, 0, 0]);

        let mut v6 = 30u32.to_be_bytes().to_vec();
        v6.extend_from_slice(&[0x60, 0, 0, 0]);
        assert_eq!(strip_af(&v6).unwrap(), &[0x60, 0, 0, 0]);

        assert!(strip_af(&[0, 0, 0]).is_none());
        assert!(strip_af(&[0, 0, 0, 7, 0x45]).is_none());
    }

    #[test]
    fn derives_the_family_from_the_packet() {
        assert_eq!(packet_family(&[0x45, 0, 0, 0]), Some(AF_INET_DARWIN));
        assert_eq!(packet_family(&[0x60, 0, 0, 0]), Some(AF_INET6_DARWIN));
        assert_eq!(packet_family(&[]), None);
        assert_eq!(packet_family(&[0x10]), None);
    }
}
