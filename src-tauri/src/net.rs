use std::collections::HashMap;

use netstat2::{get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};

/// 统计在线矿机总数：本地端口 ∈ 已打开端口集合 且 ESTABLISHED 的连接数
pub fn count_established(ports: &[u16]) -> u32 {
    count_by_port(ports).values().sum()
}

/// 按端口统计 ESTABLISHED 连接数：返回 端口 -> 连接数。
/// 用于按币种维度统计（每个矿池端口对应一个币种）。
pub fn count_by_port(ports: &[u16]) -> HashMap<u16, u32> {
    let mut map: HashMap<u16, u32> = HashMap::new();
    if ports.is_empty() {
        return map;
    }

    let af_flags = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let proto_flags = ProtocolFlags::TCP;
    let sockets = match get_sockets_info(af_flags, proto_flags) {
        Ok(s) => s,
        Err(_) => return map,
    };

    for si in sockets {
        if let ProtocolSocketInfo::Tcp(tcp) = si.protocol_socket_info {
            if tcp.state == TcpState::Established && ports.contains(&tcp.local_port) {
                *map.entry(tcp.local_port).or_insert(0) += 1;
            }
        }
    }
    map
}
