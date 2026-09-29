import {
    Std.Io.{Res as IoRes, Error as IoError}
    crate.Net.{SocketAddrV4, Ipv4Addr}
}

pub struct SocketHandle {
    socket: SOCKET

    pub This.(): IoRes<This> {
        initialize()?
        socket := unsafe {
            socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)
        }

        Ok(This{
            socket: checkSocket(socket)?,
        })
    }

    pub bind(&this, addr: SocketAddrV4): IoRes {
        rawAddr := SockAddrIn {
            family: u16.(AF_INET),
            port: addr.port().toBigEndian(),
            addr: u32.fromNativeEndian(addr.ip().octets()),
            zero: [for _ => 0_u8]
        }

        numErr := unsafe {
            bind(
                this.socket,
                *SockAddr.(*SockAddrIn.(&rawAddr)),
                i32.(SockAddrIn.sizeof),
            )
        }

        checkResult(numErr)
    }

    pub listen(&this, backlog: i32): IoRes {
        checkResult(unsafe {
            listen(this.socket, backlog)
        })
    }

    pub accept(&this): IoRes<(This, SockAddr)> {
        mut addr := SockAddrIn{..}
        mut addrLen := i32.(SockAddrIn.sizeof)

        uncheckedClient := unsafe {
            accept(
                this.socket,
                *mut SockAddr.(&mut addr),
                addrLen,
            )
        }

        client := checkSocket(uncheckedClient)?
        ip := Ipv4Addr.(addr.addr.toNativeEndianBytes())
        port := u16.fromBigEndian(addr.port)

        peerAddr := SockAddr.V4(SocketAddrV4.init(ip, port))
        Ok((
            This{socket: client},
            peerAddr,
        ))
    }

    pub localAddr(&this): IoRes<SockAddr> {
        mut addr := SockAddrIn{..}
        mut addrLen := i32.(SockAddrIn.sizeof)

        numErr := unsafe {
            getsockname(
                this.socket,
                *mut SockAddr.(&mut addr),
                *mut i32.(&mut addrLen),
            )
        }

        checkResult(numErr)?

        ip := Ipv4Addr.(addr.addr.toNativeEndianBytes())
        port := u16.fromBigEndian(addr.port)
        Ok(SockAddr.V4(SocketAddrV4.init(ip, port)))
    }

    pub recv(&this, buffer: [&mut]u8): IoRes<uint> {
        if buffer.isEmpty() {
            return OK(0)
        }

        length := i32.(
            buffer.len().min(uint.(i32.MAX))
        )

        numErr := unsafe {
            recv(
                this.socket,
                *mut i8.(&mut buffer),
                length,
                0,
            )
        }

        if numErr == SOCKET_ERROR {
            return Err(IoError.lastOsError())
        }

        Ok(uint.(numErr))
    }

    pub send(&this, buffer: [&]u8): IoRes<uint> {
        if buffer.isEmpty() {
            return OK(0)
        }

        length := i32.(
            buffer.len().min(uint.(i32.MAX))
        )

        numErr := unsafe {
            send(
                this.socket,
                *i8.(&buffer),
                length,
                0,
            )
        }

        if numErr == SOCKET_ERROR {
            return Err(IoError.lastOsError())
        }

        Ok(uint.(numErr))
    }

    pub shutdown(&this): IoRes {
        checkResult(unsafe {
            shutdown(this.socket, SD_BOTH)
        })
    }

    impl Drop drop(&mut this) {
        unsafe {
            closesocket(this.socket)
        }
    }
}

type SOCKET = uint

WINSOCK :: Once<i32>.()
initialize(): IoRes {
    numErr := *WINSOCK.getOrInit(() => {
        mut data := WASDATA{..}
        unsafe {
            WSAStartup(0x0202, *mut WSADATA.(&mut data))
        }
    })

    if numErr != 0 {
        return Err(IoError.fromOsI32(i32Res))
    }

    Ok(())
}

INVALID_SOCKET :: !SOCKET.(0)
SOCKET_ERROR :: -1_i32

AF_INET :: 2_i32
SOCK_STREAM :: 1_i32
IPPROTO_TCP :: 6_i32

SOL_SOCKET :: 0xffff_i32
SO_REUSEADDR :: 0x0004_i32

SD_BOTH :: 2_i32

#[extern("C")]
struct SockAddr {
    family: u16
    data: [14]u8
}

#[extern("C")]
struct SockAddrIn {
    family: u16 = 0
    port: u16 = 0
    addr: u32 = 0
    zero: [8]u8 = [for _ => 0_u8]
}

#[extern("C")]
struct WSADATA {
    data: [64]u64 = [for _ => 0]
}

checkSocket(socket: SOCKET): IoRes<SOCKET> {
    if socket == INVALID_SOCKET {
        return Err(IoError.lastOsError())
    }
    Ok(())
}

checkResult(numErr: i32): IoRes {
    if numErr == SOCKET_ERROR {
        return Err(IoError.lastOsError())
    }
    Ok(())
}

#[link(name = "Ws2_32")]
extern "system" {
    unsafe WSAStartup(
        version: u16,
        data: *mut WSADATA,
    ) -> i32

    unsafe WSACleanup() -> i32

    unsafe socket(
        af: i32,
        kind: i32,
        protocol: i32,
    ) -> SOCKET

    unsafe bind(
        socket: SOCKET,
        addr: *SockAddr,
        addr_len: i32,
    ) -> i32

    unsafe listen(
        socket: SOCKET,
        backlog: i32,
    ) -> i32

    unsafe accept(
        socket: SOCKET,
        addr: *mut SockAddr,
        addr_len: *mut i32,
    ) -> SOCKET

    unsafe closesocket(socket: SOCKET) -> i32

    unsafe recv(
        socket: SOCKET,
        buffer: *mut i8,
        length: i32,
        flags: i32,
    ) -> i32

    unsafe send(
        socket: SOCKET,
        buffer: *i8,
        length: i32,
        flags: i32,
    ) -> i32

    unsafe shutdown(socket: SOCKET, how: i32) -> i32

    unsafe getsockname(
        socket: SOCKET,
        addr: *mut SockAddr,
        addr_len: *mut i32,
    ) -> i32

    unsafe setsockopt(
        socket: SOCKET,
        level: i32,
        option: i32,
        value: *i8,
        value_len: i32,
    ) -> i32
}
