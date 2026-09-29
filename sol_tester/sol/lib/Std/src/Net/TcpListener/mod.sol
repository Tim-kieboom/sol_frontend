#[if(os = windows)]
import windows as native
#[if(os = unix)]
import windows as native

import {
    Std.Io.Res as IoRes
}

pub struct TcpStream {
    socket: native.SocketHandle

    pub(crate) This.(value: SocketHandle) => This{socket}
    pub shutdown(&this): IoRes => this.socket.shutdown()
}
