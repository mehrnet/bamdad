//! Windows half of the privileged desktop helper.
//!
//! On Windows the sidecar creates the device itself: xjasonlyu/tun2socks uses
//! Wintun (`golang.zx2c4.com/wireguard/tun`) and, given `-device tun://<name>`,
//! creates an adapter with that name. This helper therefore starts the payload,
//! waits for the adapter to appear, then configures the address, MTU, routes
//! and DNS with `netsh`, and undoes all of that on the way down. Windows keeps
//! DNS per interface, so removing the adapter removes its resolver config.
//!
//! Everything here runs elevated (Windows requires administrator rights to
//! create a Wintun adapter and change routes).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
    "usage: bamdad-tun-helper up --name IFACE --address ADDR/PREFIX [--mtu N] \\
        (--route CIDR | --default --bypass IP)... [--dns ADDR]... \\
        [--state FILE] [--parent-pid PID] [--exec PROGRAM [ARGS...]]
   bamdad-tun-helper down --state FILE"
        .to_string()
}

#[derive(Debug, Default)]
struct Options {
    name: String,
    address: String,
    mtu: u16,
    routes: Vec<String>,
    default: bool,
    bypass: Vec<String>,
    dns: Vec<String>,
    state: Option<PathBuf>,
    parent_pid: Option<u32>,
    exec: Vec<String>,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut options = Options {
            mtu: 1400,
            ..Default::default()
        };
        let mut rest = args.into_iter();
        while let Some(arg) = rest.next() {
            let mut value = || {
                rest.next()
                    .ok_or_else(|| format!("missing value for {arg}"))
            };
            match arg.as_str() {
                "--name" => options.name = value()?,
                "--address" => options.address = value()?,
                "--mtu" => options.mtu = value()?.parse().map_err(|_| "bad --mtu")?,
                "--route" => options.routes.push(value()?),
                "--default" => options.default = true,
                "--bypass" => options.bypass.push(value()?),
                "--dns" => options.dns.push(value()?),
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
        if options.name.is_empty() || options.address.is_empty() {
            return Err("--name and --address are required".to_string());
        }
        Ok(options)
    }
}

fn up(options: Options) -> Result<(), String> {
    if !elevated() {
        return Err("must run as administrator".to_string());
    }

    let mut child = if options.exec.is_empty() {
        None
    } else {
        Some(spawn_payload(&options.exec)?)
    };

    // The sidecar creates the adapter; wait for it before configuring.
    if !wait_for_adapter(&options.name, Duration::from_secs(15)) {
        if let Some(child) = child.as_mut() {
            let _ = child.kill();
        }
        return Err(format!("adapter {} never appeared", options.name));
    }

    let (address, prefix) = options
        .address
        .split_once('/')
        .ok_or_else(|| "--address needs a prefix, e.g. 10.255.0.1/24".to_string())?;
    netsh(&[
        "interface",
        "ipv4",
        "set",
        "address",
        "name",
        &options.name,
        "static",
        address,
        &netmask(prefix.parse().map_err(|_| "bad prefix")?),
    ])?;
    netsh(&[
        "interface",
        "ipv4",
        "set",
        "subinterface",
        &options.name,
        &format!("mtu={}", options.mtu),
        "store=active",
    ])?;

    let mut added: Vec<String> = Vec::new();
    for route in &options.routes {
        if add_route(route, &options.name).is_ok() {
            added.push(route.clone());
        }
    }
    if options.default {
        for half in ["0.0.0.0/1", "128.0.0.0/1"] {
            add_route(half, &options.name)?;
            added.push(half.to_string());
        }
        if let Some((alias, gateway)) = default_gateway() {
            for bypass in &options.bypass {
                let host = bypass.split('/').next().unwrap_or(bypass);
                let target = format!("{host}/32");
                if netsh(&[
                    "interface",
                    "ipv4",
                    "add",
                    "route",
                    &target,
                    &alias,
                    &gateway,
                    "store=active",
                ])
                .is_ok()
                {
                    added.push(format!("bypass:{target}:{alias}:{gateway}"));
                }
            }
        }
    }

    for (index, server) in options.dns.iter().enumerate() {
        let index = index + 1;
        let _ = netsh(&[
            "interface",
            "ipv4",
            "set",
            "dnsservers",
            "name",
            &options.name,
            "static",
            server,
            "primary",
        ]);
        if index > 1 {
            let _ = netsh(&[
                "interface",
                "ipv4",
                "add",
                "dnsservers",
                "name",
                &options.name,
                server,
                &format!("index={index}"),
            ]);
        }
    }

    if let Some(path) = &options.state {
        let mut text = format!(
            "device={}\npid={}\n",
            options.name,
            std::process::id()
        );
        for entry in &added {
            text.push_str(&format!("route={entry}\n"));
        }
        fs::write(path, text).map_err(|error| format!("state file: {error}"))?;
    }

    println!("ready {}", options.name);
    let _ = std::io::stdout().flush();

    install_ctrl_handler();

    while !STOP.load(Ordering::SeqCst) {
        if let Some(child) = child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
        }
        if let Some(pid) = options.parent_pid {
            if !process_alive(pid) {
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

    for entry in &added {
        let _ = remove_route(entry, &options.name);
    }
    if let Some(path) = &options.state {
        let _ = fs::remove_file(path);
    }
    println!("stopped");
    Ok(())
}

fn down(options: Options) -> Result<(), String> {
    let path = options
        .state
        .ok_or_else(|| "down needs --state FILE".to_string())?;
    let text = fs::read_to_string(&path).map_err(|error| format!("{error}"))?;
    let mut device = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("device=") {
            device = value.to_string();
        }
    }
    for line in text.lines() {
        if let Some(entry) = line.strip_prefix("route=") {
            let _ = remove_route(entry, &device);
        } else if let Some(pid) = line.strip_prefix("pid=") {
            if let Ok(pid) = pid.parse::<u32>() {
                if pid != std::process::id() {
                    let _ = Command::new("taskkill")
                        .args(["/F", "/PID", &pid.to_string()])
                        .output();
                }
            }
        }
    }
    let _ = fs::remove_file(&path);
    println!("stopped");
    Ok(())
}

fn add_route(cidr: &str, device: &str) -> Result<(), String> {
    netsh(&[
        "interface",
        "ipv4",
        "add",
        "route",
        cidr,
        device,
        "store=active",
    ])
    .map(|_| ())
}

fn remove_route(entry: &str, device: &str) -> Result<(), String> {
    if let Some(rest) = entry.strip_prefix("bypass:") {
        let mut parts = rest.split(':');
        let target = parts.next().unwrap_or_default();
        let alias = parts.next().unwrap_or_default();
        let gateway = parts.next().unwrap_or_default();
        return netsh(&[
            "interface",
            "ipv4",
            "delete",
            "route",
            target,
            alias,
            gateway,
            "store=active",
        ])
        .map(|_| ());
    }
    netsh(&[
        "interface",
        "ipv4",
        "delete",
        "route",
        entry,
        device,
        "store=active",
    ])
    .map(|_| ())
}

fn default_gateway() -> Option<(String, String)> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Select-Object -First 1 | ForEach-Object { \"$($_.InterfaceAlias)|$($_.NextHop)\" })",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.trim();
    let (alias, gateway) = line.split_once('|')?;
    Some((alias.trim().to_string(), gateway.trim().to_string()))
}

fn netmask(prefix: u8) -> String {
    let mask: u32 = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix.min(32))
    };
    format!(
        "{}.{}.{}.{}",
        (mask >> 24) & 0xff,
        (mask >> 16) & 0xff,
        (mask >> 8) & 0xff,
        mask & 0xff
    )
}

fn spawn_payload(argv: &[String]) -> Result<Child, String> {
    Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", argv[0]))
}

fn wait_for_adapter(name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if netsh(&["interface", "ipv4", "show", "interfaces", name]).is_ok()
            && netsh(&["interface", "ipv4", "show", "addresses", name]).is_ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn netsh(args: &[&str]) -> Result<String, String> {
    let output = Command::new("netsh")
        .args(args)
        .output()
        .map_err(|error| format!("netsh {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "netsh {} failed: {}",
            args.join(" "),
            stderr.trim()
        ));
    }
    if stdout.contains("The filename, directory name, or volume label syntax is incorrect") {
        return Err(format!("netsh {} failed", args.join(" ")));
    }
    Ok(stdout.to_string())
}

fn elevated() -> bool {
    // Creating a Wintun adapter fails without administrator rights; probing the
    // token through `net session` is the classic cheap check.
    Command::new("net")
        .arg("session")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn process_alive(pid: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output();
    match output {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            text.contains(&pid.to_string())
        }
        Err(_) => false,
    }
}

fn install_ctrl_handler() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{BOOL, TRUE};
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

        unsafe extern "system" fn handler(_: u32) -> BOOL {
            STOP.store(true, Ordering::SeqCst);
            TRUE
        }
        unsafe {
            SetConsoleCtrlHandler(Some(handler), TRUE);
        }
    }
}
