crate.Net.{SocketAddrV4, SockAddr, Ipv4Addr}

pub struct Ipv4Addr {
    data: [4]u8

    This.(value: [&]u8) => This{data: [for el in &value[..3] => *el]}
    This.[4]u8(data) => This{data}
    pub asSlice(&this): &[4]u8 => &this.data
}

pub struct Ipv6Addr {
    data: [6]u8

    This.(value: [&]u8) => This{data: [for el in &value[..5] => *el]}
    This.[6]u8(data) => This{data}
    pub asSlice(&this): &[6]u8 => &this.data
}

pub struct SocketAddrV4 {
    pub ip: Ipv4Addr,
    pub port: u16,
    pub init(ip: Ipv4Addr, port: u16): This => This{ip, port}
}

pub struct SocketAddrV6 {
    pub ip: Ipv6Addr,
    pub port: u16,
    pub init(ip: Ipv6Addr, port: u16): This => This{ip, port}
}

pub enum SocketAddr {
    V4(SockAddrV4),
    V6(SockAddrV6),
}
