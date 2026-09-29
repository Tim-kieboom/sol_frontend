import {
    Std.Io.{Res as IoRes, Error as IoError}
    crate.Net.{SocketAddrV4, Ipv4Addr}
}

type RawFd = cint
type Socklen_t = uint

AF_INET :: 2_i32
SOCK_STREAM :: 1_i32
IPPROTO_TCP :: 6_i32

#[extern("C")]
struct SockAddr {
    family: u16 = 0
    data: [14]u8 = [for _ => 0]
}

#[extern("C")]
struct SockAddrIn {
    family: u16 = 0
    port: u16 = 0
    addr: u32 = 0
    zero: [8]u8 = [for _ => 0]
}

pub struct SocketHandle {
    fb: RawFd

    This.(): IoRes<This> {
        fd := unsafe {
            Libc.socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)
        }

        if fd < 0 {
            return Err(IoError.lastOsError())
        }

        Ok(This{fd})
    }

    pub bind(&this, addr: SocketAddrV4): IoRes {
        rawAddr := SockaddrIn {
            sinFamily: AF_NET as Libc.sa_famaly_t,
            sinPort: addr.port.toBigEndian()
            sinAddr: Libc.inAddr{
                sAddr: u32.fromNativeBytes(addr.ip.octets())
            }
            sinZero: [for _ => 0]
        }

        numErr := unsafe {
            Libc.bind(
                this.fb,
                *Sockaddr.(*Socklen_t.(&rawAddr)),
                socklen_t.(SockaddrIn.sizeof)
            )
        }

        if numErr < 0 {
            return Err(IoError.lastOsError())
        }

        Ok(())
    }

    pub listen(&this, backlog: i32): IoRes {
        numErr := unsafe {
            Libc.listen(this.fb, backlog)
        }

        if numErr < 0 {
            return Err(IoError.lastOsError())
        }

        Ok(())
    }

    pub accept(&this): IoRes<(This, SockAddr)> {
        mut addr := SockaddrIn.()
        mut addrLen := Socklen_t.(SockaddrIn.sizeof)
        fd := unsafe {
            Libc.accept(
                this.fd,
                *mut Sockaddr.(*mut SockaddrIn.(&mut addr)),
                &mut addrLen
            )
        }

        if numErr < 0 {
            return Err(IoError.lastOsError())
        }

        id := Ipv4Addr.(addr.sinAddr.sAddr.toNativeEndianBytes())
        port := u16.fromBigEndian(addr.sinPort)

        peer := SocketAddr.V4(SocketAddrV4.init(ip, port))

        Ok((
            This{fd},
            peer
        ))
    }

    pub revc(&this, buffer: &mut [u8]): IoRes<uint> {
        numErr := unsafe {
            Libc.revc(
                this.fd,
                RawPtr.(&mut buffer)
            )
        }

        if numErr < 0 {
            return Err(IoError.lastOsError())
        }

        Ok(uint.(numErr))
    }

    pub send(&this, buffer: [&]u8): IoRes<uint> {
        numErr := unsafe {
            Libc.send(
                this.fd,
                RawPtr.(&buffer),
                buffer.len(),
                0,
            )
        }

        if numErr < 0 {
            return Err(IoError.lastOsError())
        }

        Ok(uint.(numErr))
    }

    impl Drop drop(&mut this) {
        unsafe {
            Libc.close(this.fd)
        }
    }
}
