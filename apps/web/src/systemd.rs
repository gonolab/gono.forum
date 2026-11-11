//! The systemd notification protocol used by the `Type=notify` units forum.install.sh writes: readiness, a status line,
//! and extending the start timeout while migrations run. Without `NOTIFY_SOCKET` (source builds, manual starts)
//! every call does nothing.

/// Sends one notification, e.g. `READY=1` or `STATUS=…`. Failures are only logged: the service keeps running and
/// systemd reports the missing readiness itself.
pub fn notify(state: &str) {
    #[cfg(unix)]
    if let Some(path) = std::env::var_os("NOTIFY_SOCKET")
        && let Err(error) = send(&path, state)
    {
        tracing::warn!(%error, "systemd notification failed");
    }
    #[cfg(not(unix))]
    let _ = state;
}

/// Asks for `seconds` more of start time; systemd only honours it before `READY=1`.
pub fn extend_start(seconds: u64) {
    notify(&format!(
        "EXTEND_TIMEOUT_USEC={}",
        seconds.saturating_mul(1_000_000)
    ));
}

#[cfg(unix)]
fn send(path: &std::ffi::OsStr, state: &str) -> std::io::Result<()> {
    use std::os::unix::{ffi::OsStrExt, net::UnixDatagram};
    let socket = UnixDatagram::unbound()?;
    if let Some(name) = path.as_bytes().strip_prefix(b"@") {
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt;
            let address = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
            socket.send_to_addr(state.as_bytes(), &address)?;
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            return Err(std::io::Error::other(
                "abstract notification sockets need Linux",
            ));
        }
    }
    socket.send_to(state.as_bytes(), path)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn notifications_reach_a_path_socket() {
        let dir = std::env::temp_dir().join(format!(
            "gonof-notify-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify");
        let receiver = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        super::send(path.as_os_str(), "READY=1").unwrap();
        let mut buffer = [0_u8; 64];
        let length = receiver.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], b"READY=1");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
