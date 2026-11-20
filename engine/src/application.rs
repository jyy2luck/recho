use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Application {
    pub pid: i32,
    pub name: String,
    pub path: String,
}

pub async fn identify(peer: SocketAddr, local: SocketAddr) -> Option<Application> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        use std::sync::OnceLock;
        use tokio::sync::Semaphore;
        static LOOKUPS: OnceLock<std::sync::Arc<Semaphore>> = OnceLock::new();
        // Bound the blocking OS queries even under a burst of new connections.
        let permit = LOOKUPS
            .get_or_init(|| std::sync::Arc::new(Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
            .ok()?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            lookup(peer, local)
        })
        .await
        .ok()
        .flatten()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (peer, local);
        None
    }
}

#[cfg(target_os = "macos")]
fn lookup(peer: SocketAddr, local: SocketAddr) -> Option<Application> {
    use std::ffi::{CStr, c_char};
    unsafe extern "C" {
        fn recho_socket_application(
            client_ip: u32,
            client_port: u16,
            proxy_ip: u32,
            proxy_port: u16,
            path: *mut c_char,
            path_size: u32,
            name: *mut c_char,
            name_size: u32,
        ) -> i32;
    }
    let (SocketAddr::V4(peer), SocketAddr::V4(local)) = (peer, local) else {
        return None;
    };
    let mut path = [0 as c_char; 4096];
    let mut name = [0 as c_char; 1024];
    // The C helper uses SDK structs, checks their returned sizes, and writes
    // only to these bounded, zero-terminated buffers. Addresses are wire order.
    let pid = unsafe {
        recho_socket_application(
            u32::from_ne_bytes(peer.ip().octets()),
            peer.port().to_be(),
            u32::from_ne_bytes(local.ip().octets()),
            local.port().to_be(),
            path.as_mut_ptr(),
            path.len() as u32,
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if pid <= 0 {
        return None;
    }
    Some(Application {
        pid,
        name: unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
        path: unsafe { CStr::from_ptr(path.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    })
}

#[cfg(target_os = "windows")]
fn lookup(peer: SocketAddr, local: SocketAddr) -> Option<Application> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
        },
        Networking::WinSock::AF_INET,
        System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
    };
    let (SocketAddr::V4(peer), SocketAddr::V4(local)) = (peer, local) else {
        return None;
    };
    let mut size = 0;
    unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
    }
    if size < 4 || size > 16 * 1024 * 1024 {
        return None;
    }
    // DWORD storage provides the alignment required by the Windows table structures.
    let mut table = vec![0u32; (size as usize).div_ceil(4)];
    let capacity = table.len() * 4;
    if unsafe {
        GetExtendedTcpTable(
            table.as_mut_ptr().cast(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    } != 0
    {
        return None;
    }
    let count = table[0] as usize;
    if 4 + count.checked_mul(std::mem::size_of::<MIB_TCPROW_OWNER_PID>())? > capacity {
        return None;
    }
    for index in 0..count {
        let row = unsafe {
            std::ptr::read_unaligned(
                table
                    .as_ptr()
                    .cast::<u8>()
                    .add(4 + index * std::mem::size_of::<MIB_TCPROW_OWNER_PID>())
                    .cast::<MIB_TCPROW_OWNER_PID>(),
            )
        };
        if row.dwLocalAddr != u32::from_ne_bytes(peer.ip().octets())
            || u16::from_be(row.dwLocalPort as u16) != peer.port()
            || row.dwRemoteAddr != u32::from_ne_bytes(local.ip().octets())
            || u16::from_be(row.dwRemotePort as u16) != local.port()
        {
            continue;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, row.dwOwningPid) };
        if process.is_null() {
            return None;
        }
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
        unsafe {
            CloseHandle(process);
        }
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&path[..length as usize]);
        let name = std::path::Path::new(&path)
            .file_stem()?
            .to_string_lossy()
            .into_owned();
        return Some(Application {
            pid: row.dwOwningPid as i32,
            name,
            path,
        });
    }
    None
}

#[cfg(target_os = "linux")]
fn proc_address(value: &str) -> Option<SocketAddr> {
    let (address, port) = value.split_once(':')?;
    let word = |s| u32::from_str_radix(s, 16).ok().map(u32::to_ne_bytes);
    let ip = match address.len() {
        8 => std::net::IpAddr::V4(word(address)?.into()),
        32 => {
            let mut bytes = [0u8; 16];
            for (index, chunk) in bytes.chunks_exact_mut(4).enumerate() {
                chunk.copy_from_slice(&word(address.get(index * 8..index * 8 + 8)?)?);
            }
            std::net::IpAddr::V6(bytes.into())
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, u16::from_str_radix(port, 16).ok()?))
}

#[cfg(target_os = "linux")]
fn lookup(peer: SocketAddr, local: SocketAddr) -> Option<Application> {
    use std::{
        fs,
        io::{BufRead, BufReader, Read},
        os::unix::fs::MetadataExt,
        time::{Duration, Instant},
    };
    let deadline = Instant::now() + Duration::from_millis(150);
    let table = if peer.is_ipv4() { "tcp" } else { "tcp6" };
    let file = fs::File::open(format!("/proc/net/{table}")).ok()?;
    let mut inode = None;
    for line in BufReader::new(file.take(4 * 1024 * 1024)).lines().skip(1) {
        if Instant::now() >= deadline {
            return None;
        }
        let line = line.ok()?;
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() > 9
            && fields[3] == "01"
            && proc_address(fields[1]) == Some(peer)
            && proc_address(fields[2]) == Some(local)
        {
            let value = fields[9].parse::<u64>().ok()?;
            if value != 0 {
                inode = Some(format!("socket:[{value}]"));
                break;
            }
        }
    }
    let inode = inode?;
    let uid = fs::metadata("/proc/self").ok()?.uid();
    let current = std::process::id();
    let processes = fs::read_dir("/proc")
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok());
    let mut descriptors = 0;
    // shortcut: bounded proc scans can miss transient sockets; use inet_diag if stronger coverage is needed.
    for pid in std::iter::once(current).chain(processes.filter(|pid| *pid != current)) {
        if Instant::now() >= deadline {
            return None;
        }
        let directory = std::path::PathBuf::from(format!("/proc/{pid}"));
        if fs::metadata(&directory).ok().is_none_or(|v| v.uid() != uid) {
            continue;
        }
        let Ok(fds) = fs::read_dir(directory.join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            descriptors += 1;
            if descriptors > 16_384 || Instant::now() >= deadline {
                return None;
            }
            if !fs::read_link(fd.path()).is_ok_and(|v| v.to_str() == Some(inode.as_str())) {
                continue;
            }
            let path = fs::read_link(directory.join("exe")).ok()?;
            if fs::read_link(fd.path()).ok()?.to_str() != Some(inode.as_str()) {
                return None;
            }
            return Some(Application {
                pid: i32::try_from(pid).ok()?,
                name: path.file_stem()?.to_string_lossy().into_owned(),
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
    None
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
mod tests {
    #[tokio::test]
    async fn identifies_the_client_socket_not_the_proxy_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, peer) = listener.accept().await.unwrap();
        let app = super::identify(peer, server.local_addr().unwrap())
            .await
            .expect("current user's client must be readable");
        assert_eq!(app.pid, std::process::id() as i32);
        assert_eq!(app.path, std::env::current_exe().unwrap().to_str().unwrap());
        assert!(
            super::identify(
                "192.0.2.1:1234".parse().unwrap(),
                server.local_addr().unwrap()
            )
            .await
            .is_none()
        );
        drop(client);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_kernel_socket_addresses_and_rejects_malformed_rows() {
        assert_eq!(
            super::proc_address("0100007F:1F90"),
            Some("127.0.0.1:8080".parse().unwrap())
        );
        assert_eq!(
            super::proc_address("00000000000000000000000001000000:01BB"),
            Some("[::1]:443".parse().unwrap())
        );
        for invalid in ["", "0100007F", "GG00007F:0050", "0100007F:10000"] {
            assert!(super::proc_address(invalid).is_none());
        }
    }
}
