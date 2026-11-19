//! Self-fetch relabeling (`ngx_self_fetch_label.*`).
//!
//! Internal fetches from loopback must not inherit localhost's
//! address-based access-rule privileges, so the post-read
//! handler temporarily relabels the connection's peer address
//! to 224.0.0.0 and restores it at request cleanup. All three
//! routines are pure address surgery, unit-tested here over
//! fabricated socket addresses.

use ngx::ffi::ngx_connection_t;
use std::mem;

/// Multicast label address (`0xe0000000` = 224.0.0.0).
pub const LABEL_ADDR: u32 = 0xe0000000;

/// Label text (`"224.0.0.0"`, NUL-terminated in the buffer).
pub const LABEL_TEXT: &[u8; 10] = b"224.0.0.0\0";

/// Relabel scratch (`NgxSelfFetchLabel`).
#[derive(Debug, Clone, Copy)]
pub struct SelfFetchLabel {
    /// Replacement peer address (AF_INET 224.0.0.0).
    pub addr: libc::sockaddr_in,
    /// Replacement address text (NUL-terminated).
    pub text: [u8; 10],
}

/// Saved peer address (`NgxSelfFetchSaved<ngx_connection_t>`).
#[derive(Debug, Clone, Copy)]
pub struct SavedPeer {
    /// Connection the values were saved from.
    pub connection: *mut ngx_connection_t,
    /// Saved peer address pointer.
    pub sockaddr: *mut libc::sockaddr,
    /// Saved peer address length.
    pub socklen: libc::socklen_t,
    /// Saved address text length.
    pub addr_text_len: usize,
    /// Saved address text pointer.
    pub addr_text_data: *mut u8,
}

// SAFETY: plain snapshot of raw parts; restored, never dereferenced.
unsafe impl Send for SavedPeer {}
unsafe impl Sync for SavedPeer {}

/// Whether an address is loopback (`IsLoopback`).
///
/// # Safety
///
/// `address`, when non-null, must point at a readable socket
/// address whose family field is valid.
#[must_use]
pub unsafe fn is_loopback(address: *const libc::sockaddr) -> bool {
    if address.is_null() {
        return false;
    }
    // SAFETY: caller guarantees a readable address.
    let family = unsafe { (*address).sa_family };
    if family as i32 == libc::AF_INET {
        // SAFETY: family says AF_INET, so the IPv4 view is valid.
        let ipv4 = unsafe { &*(address as *const libc::sockaddr_in) };
        (u32::from_be(ipv4.sin_addr.s_addr) >> 24) == 127
    } else if family as i32 == libc::AF_INET6 {
        // SAFETY: family says AF_INET6, so the IPv6 view is valid.
        let ipv6 = unsafe { &*(address as *const libc::sockaddr_in6) };
        // `IN6_IS_ADDR_LOOPBACK`: all zeros except the last byte.
        let mut expected = [0u8; 16];
        expected[15] = 1;
        ipv6.sin6_addr.s6_addr == expected
    } else {
        false
    }
}

/// Whether the peer is this server (`NgxSelfFetchPeerIsThisServer`).
///
/// # Safety
///
/// Non-null pointers must be readable socket addresses with
/// valid family fields, sized for their families.
#[must_use]
pub unsafe fn peer_is_this_server(
    peer: *const libc::sockaddr,
    local: *const libc::sockaddr,
) -> bool {
    if peer.is_null() {
        return false;
    }
    // SAFETY: pointers as documented.
    unsafe {
        if is_loopback(peer) {
            return true;
        }
        if local.is_null() || (*local).sa_family != (*peer).sa_family {
            return false;
        }
        if (*peer).sa_family as i32 == libc::AF_INET {
            let p = &*(peer as *const libc::sockaddr_in);
            let l = &*(local as *const libc::sockaddr_in);
            p.sin_addr.s_addr == l.sin_addr.s_addr
        } else if (*peer).sa_family as i32 == libc::AF_INET6 {
            let p = &*(peer as *const libc::sockaddr_in6);
            let l = &*(local as *const libc::sockaddr_in6);
            p.sin6_addr.s6_addr == l.sin6_addr.s6_addr
        } else {
            false
        }
    }
}

/// Initializes a relabel scratch (`NgxInitSelfFetchLabel`).
pub fn init_label(label: &mut SelfFetchLabel) {
    // SAFETY: zeroing a caller-owned struct, as the C++ `memset`.
    unsafe {
        ptr_write_zeroed(label);
    }
    label.addr.sin_family = libc::AF_INET as u16;
    label.addr.sin_addr.s_addr = LABEL_ADDR.to_be();
    label.text = *LABEL_TEXT;
}

/// Zeroes a struct through a mutable reference (`memset`).
///
/// # Safety
///
/// Valid for structs whose all-zero bit pattern is valid, as
/// with the C++ `memset` this mirrors.
unsafe fn ptr_write_zeroed<T>(target: &mut T) {
    // SAFETY: caller guarantees the zero pattern is valid.
    unsafe {
        std::ptr::write_bytes(target as *mut T as *mut u8, 0, mem::size_of::<T>());
    }
}

/// Relabels a connection to 224.0.0.0 (`NgxRelabelSelfFetch`).
///
/// Bindgen and libc declare distinct socket types with identical
/// C layouts; the casts below cross that boundary (pinned by
/// `socket_layouts_match_bindgen`).
///
/// # Safety
///
/// `connection` must point at a live `ngx_connection_t`;
/// `label` and `saved` must be caller-owned and stay alive
/// until `restore_self_fetch` runs at request cleanup.
pub unsafe fn relabel_self_fetch(
    connection: *mut ngx_connection_t,
    label: *mut SelfFetchLabel,
    saved: *mut SavedPeer,
) {
    // SAFETY: caller guarantees all three pointers as documented.
    unsafe {
        (*saved).connection = connection;
        (*saved).sockaddr = (*connection).sockaddr as *mut libc::sockaddr;
        (*saved).socklen = (*connection).socklen as libc::socklen_t;
        (*saved).addr_text_len = (*connection).addr_text.len;
        (*saved).addr_text_data = (*connection).addr_text.data;
        init_label(&mut *label);
        (*connection).sockaddr = &mut (*label).addr as *mut libc::sockaddr_in
            as *mut ngx::ffi::sockaddr;
        (*connection).socklen = mem::size_of::<libc::sockaddr_in>() as ngx::ffi::socklen_t;
        (*connection).addr_text.data = (*label).text.as_mut_ptr();
        (*connection).addr_text.len = (*label).text.len() - 1;
    }
}

/// Restores a relabeled connection (`NgxRestoreSelfFetch`).
///
/// # Safety
///
/// `saved` must hold values from `relabel_self_fetch` whose
/// connection is still live.
pub unsafe fn restore_self_fetch(saved: &SavedPeer) {
    // SAFETY: caller guarantees the saved snapshot is live.
    unsafe {
        let connection = saved.connection;
        (*connection).sockaddr = saved.sockaddr as *mut ngx::ffi::sockaddr;
        (*connection).socklen = saved.socklen as ngx::ffi::socklen_t;
        (*connection).addr_text.len = saved.addr_text_len;
        (*connection).addr_text.data = saved.addr_text_data;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> libc::sockaddr_in {
        let mut addr: libc::sockaddr_in = unsafe { mem::zeroed() };
        addr.sin_family = libc::AF_INET as u16;
        addr.sin_addr.s_addr = u32::from_ne_bytes([a, b, c, d]);
        addr
    }

    fn v6(last: u8) -> libc::sockaddr_in6 {
        let mut addr: libc::sockaddr_in6 = unsafe { mem::zeroed() };
        addr.sin6_family = libc::AF_INET6 as u16;
        addr.sin6_addr.s6_addr[15] = last;
        addr
    }

    #[test]
    fn loopback_detection() {
        // SAFETY: stack addresses outlive the calls.
        unsafe {
            assert!(!is_loopback(std::ptr::null()));
            let lo = v4(127, 0, 0, 1);
            assert!(is_loopback(&lo as *const _ as *const libc::sockaddr));
            let wide = v4(10, 0, 0, 1);
            assert!(!is_loopback(&wide as *const _ as *const libc::sockaddr));
            // Any 127/8 address counts (high octet only).
            let whole = v4(127, 9, 9, 9);
            assert!(is_loopback(&whole as *const _ as *const libc::sockaddr));
            let lo6 = v6(1);
            assert!(is_loopback(&lo6 as *const _ as *const libc::sockaddr));
            let not6 = v6(2);
            assert!(!is_loopback(&not6 as *const _ as *const libc::sockaddr));
        }
    }

    #[test]
    fn peer_matching() {
        // SAFETY: stack addresses outlive the calls.
        unsafe {
            let lo = v4(127, 0, 0, 1);
            let lo_ptr = &lo as *const _ as *const libc::sockaddr;
            // Null peer never matches; loopback always does, even
            // with a null or mismatched local side.
            assert!(!peer_is_this_server(std::ptr::null(), lo_ptr));
            assert!(peer_is_this_server(lo_ptr, std::ptr::null()));
            let other = v4(10, 1, 2, 3);
            let other_ptr = &other as *const _ as *const libc::sockaddr;
            assert!(!peer_is_this_server(other_ptr, std::ptr::null()));
            // Same-family equality decides.
            let same = v4(10, 1, 2, 3);
            assert!(peer_is_this_server(
                other_ptr,
                &same as *const _ as *const libc::sockaddr
            ));
            let different = v4(10, 1, 2, 4);
            assert!(!peer_is_this_server(
                other_ptr,
                &different as *const _ as *const libc::sockaddr
            ));
            // Cross-family never matches.
            let v6addr = v6(1);
            assert!(!peer_is_this_server(
                other_ptr,
                &v6addr as *const _ as *const libc::sockaddr
            ));
            // IPv6 compares all 16 bytes.
            let a6 = v6(5);
            let b6 = v6(5);
            let c6 = v6(6);
            assert!(peer_is_this_server(
                &a6 as *const _ as *const libc::sockaddr,
                &b6 as *const _ as *const libc::sockaddr
            ));
            assert!(!peer_is_this_server(
                &a6 as *const _ as *const libc::sockaddr,
                &c6 as *const _ as *const libc::sockaddr
            ));
        }
    }

    #[test]
    fn label_init_matches_cxx() {
        let mut label: SelfFetchLabel = unsafe { mem::zeroed() };
        label.text = [0xAA; 10];
        init_label(&mut label);
        assert_eq!(libc::AF_INET as u16, label.addr.sin_family);
        assert_eq!(LABEL_ADDR, u32::from_be(label.addr.sin_addr.s_addr));
        assert_eq!(0, label.addr.sin_port);
        assert_eq!(*LABEL_TEXT, label.text);
    }

    #[test]
    fn socket_layouts_match_bindgen() {
        assert_eq!(mem::size_of::<libc::sockaddr>(), mem::size_of::<ngx::ffi::sockaddr>());
        assert_eq!(
            mem::size_of::<libc::sockaddr_in>(),
            mem::size_of::<ngx::ffi::sockaddr_in>()
        );
        assert_eq!(
            mem::size_of::<libc::sockaddr_in6>(),
            mem::size_of::<ngx::ffi::sockaddr_in6>()
        );
        assert_eq!(libc::AF_INET, ngx::ffi::AF_INET as i32);
        assert_eq!(libc::AF_INET6, ngx::ffi::AF_INET6 as i32);
    }

    #[test]
    fn relabel_round_trip() {
        // SAFETY: all structures are test-owned; no nginx involved.
        unsafe {
            let mut conn: ngx_connection_t = MaybeUninit::zeroed().assume_init();
            let mut peer = v4(10, 0, 0, 9);
            let mut text = *b"10.0.0.9\0\0\0";
            conn.sockaddr = &mut peer as *mut _ as *mut ngx::ffi::sockaddr;
            conn.socklen = mem::size_of::<libc::sockaddr_in>() as ngx::ffi::socklen_t;
            conn.addr_text.data = text.as_mut_ptr();
            conn.addr_text.len = 8;
            let mut label: SelfFetchLabel = mem::zeroed();
            let mut saved: SavedPeer = mem::zeroed();
            relabel_self_fetch(&mut conn, &mut label, &mut saved);
            assert_eq!(LABEL_ADDR, u32::from_be(label.addr.sin_addr.s_addr));
            assert_eq!(
                &mut label.addr as *mut _ as *mut ngx::ffi::sockaddr,
                conn.sockaddr
            );
            assert_eq!(label.text.as_mut_ptr(), conn.addr_text.data);
            assert_eq!(9, conn.addr_text.len);
            assert_eq!(b"224.0.0.0", &label.text[..9]);
            restore_self_fetch(&saved);
            assert_eq!(
                &mut peer as *mut _ as *mut ngx::ffi::sockaddr,
                conn.sockaddr
            );
            assert_eq!(text.as_mut_ptr(), conn.addr_text.data);
            assert_eq!(8, conn.addr_text.len);
        }
    }
}
