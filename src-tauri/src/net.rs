use netstat2::{get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};

/// 统计在线矿机：本地端口 ∈ 已打开端口集合 且状态为 ESTABLISHED 的 TCP 连接数。
/// 使用 netstat2（Windows 走 IPHLPAPI / Linux 走 /proc/net/tcp / macOS 走 sysctl），
/// 三个平台统计口径一致。gost 转发到上游的连接用随机临时端口，天然不会被误计。
pub fn count_established(ports: &[u16]) -> u32 {
    if ports.is_empty() {
        return 0;
    }

    let af_flags = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let proto_flags = ProtocolFlags::TCP;
    let sockets = match get_sockets_info(af_flags, proto_flags) {
        Ok(s) => s,
        Err(_) => return 0,
    };

    let mut count: u32 = 0;
    for si in sockets {
        if let ProtocolSocketInfo::Tcp(tcp) = si.protocol_socket_info {
            if tcp.state == TcpState::Established && ports.contains(&tcp.local_port) {
                count += 1;
            }
        }
    }
    count
}
