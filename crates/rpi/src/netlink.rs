//! netlink, the socket protocol that configures the linux network stack.
//! a request is a header, a fixed part that depends on the family, and
//! attributes. everything is in native byte order and padded to 4 bytes.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const HEADER_BYTES: usize = 16;
const ATTR_HEADER_BYTES: usize = 4;
const ALIGN: usize = 4;
/// room for one datagram of a dump
const RECV_BYTES: usize = 32 * 1024;
/// attribute types carry two flag bits
const ATTR_TYPE_MASK: u16 = 0x3fff;

pub struct Socket {
    fd: OwnedFd,
    sequence: u32,
}

fn checked(result: isize) -> io::Result<usize> {
    usize::try_from(result).map_err(|_| io::Error::last_os_error())
}

fn padded(len: usize) -> usize {
    len.next_multiple_of(ALIGN)
}

/// one attribute: length, type, value, padding.
pub fn attr(kind: u16, value: &[u8]) -> Vec<u8> {
    let length = (ATTR_HEADER_BYTES + value.len()) as u16;
    let mut attr = [&length.to_ne_bytes()[..], &kind.to_ne_bytes(), value].concat();
    attr.resize(padded(attr.len()), 0);
    attr
}

/// the (type, value) pairs of a run of attributes.
pub fn attrs(mut data: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        let length = usize::from(u16::from_ne_bytes([*data.first()?, *data.get(1)?]));
        let kind = u16::from_ne_bytes([*data.get(2)?, *data.get(3)?]) & ATTR_TYPE_MASK;
        let value = data.get(ATTR_HEADER_BYTES..length)?;
        data = data.get(padded(length)..).unwrap_or_default();
        Some((kind, value))
    })
}

fn u32_at(data: &[u8], pos: usize) -> Option<u32> {
    data.get(pos..pos + 4).and_then(|bytes| bytes.try_into().ok()).map(u32::from_ne_bytes)
}

impl Socket {
    /// opens a socket to the kernel side of `protocol`.
    pub fn open(protocol: i32) -> io::Result<Socket> {
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, protocol) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Socket { fd: unsafe { OwnedFd::from_raw_fd(fd) }, sequence: 0 })
    }

    /// sends one message of type `kind` and returns the payloads of the
    /// replies, up to the acknowledgement or the end of a dump. an error
    /// that the kernel reports becomes the error of the call.
    pub fn request(&mut self, kind: u16, flags: i32, payload: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        self.sequence += 1;
        let flags = (libc::NLM_F_REQUEST | libc::NLM_F_ACK | flags) as u16;
        let length = (HEADER_BYTES + payload.len()) as u32;
        let (sequence, port) = (self.sequence.to_ne_bytes(), 0u32.to_ne_bytes());
        let message =
            [&length.to_ne_bytes()[..], &kind.to_ne_bytes(), &flags.to_ne_bytes(), &sequence, &port, payload]
                .concat();
        checked(unsafe { libc::send(self.fd.as_raw_fd(), message.as_ptr().cast(), message.len(), 0) })?;
        let (mut replies, mut buf) = (Vec::new(), vec![0u8; RECV_BYTES]);
        loop {
            let received = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if self.collect(&buf[..checked(received)?], &mut replies)? {
                return Ok(replies);
            }
        }
    }

    /// takes the messages of one datagram. true once the reply is complete.
    fn collect(&self, mut data: &[u8], replies: &mut Vec<Vec<u8>>) -> io::Result<bool> {
        while let Some(length) = u32_at(data, 0).map(|length| length as usize) {
            let Some(message) = data.get(..length).filter(|_| length >= HEADER_BYTES) else { break };
            let kind = i32::from(u16::from_ne_bytes([message[4], message[5]]));
            let payload = &message[HEADER_BYTES..];
            data = data.get(padded(length)..).unwrap_or_default();
            if u32_at(message, 8) != Some(self.sequence) {
                continue;
            }
            match kind {
                libc::NLMSG_DONE => return Ok(true),
                libc::NLMSG_ERROR => {
                    return match u32_at(payload, 0).map_or(0, |code| code as i32) {
                        0 => Ok(true),
                        code => Err(io::Error::from_raw_os_error(-code)),
                    };
                }
                _ => replies.push(payload.to_vec()),
            }
        }
        Ok(false)
    }
}
