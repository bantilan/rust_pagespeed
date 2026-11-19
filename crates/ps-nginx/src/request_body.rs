//! Loading a client request body that nginx spooled to a
//! temporary file into one contiguous buffer, for the beacon
//! and admin POST handlers.

/// The largest request body the module loads into memory,
/// matching the Apache port's limit for the same handlers
/// (`kNgxMaxRequestBodyBytes`).
pub const MAX_REQUEST_BODY_BYTES: usize = 131072;

/// Read outcome (`NgxRequestBodyStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestBodyStatus {
    /// The whole body was read.
    Ok,
    /// The read failed.
    ReadError,
    /// The body exceeds the byte budget.
    TooLarge,
}

/// Reads the whole body through `read` into `body`, never
/// holding more than `max_bytes` of it (`NgxReadRequestBody`).
/// The reader fills `buf` from `offset` and returns the
/// number of bytes read, 0 at the end of the body, or a
/// negative value on error.
pub fn read_request_body(
    read: &mut dyn FnMut(&mut [u8; 65536], i64) -> i64,
    max_bytes: usize,
    body: &mut Vec<u8>,
) -> RequestBodyStatus {
    const READ_CHUNK_BYTES: usize = 65536;
    let mut buf = [0u8; READ_CHUNK_BYTES];
    let mut offset: i64 = 0;
    loop {
        let ret = read(&mut buf, offset);
        if ret <= 0 {
            if ret < 0 {
                return RequestBodyStatus::ReadError;
            }
            return RequestBodyStatus::Ok;
        }
        if ret as u64 > READ_CHUNK_BYTES as u64 {
            return RequestBodyStatus::ReadError;
        }
        // Stop before holding more than max_bytes, rather than spooling
        // the whole file into memory first.
        if ret as u64 > (max_bytes as u64).wrapping_sub(body.len() as u64) {
            return RequestBodyStatus::TooLarge;
        }
        body.extend_from_slice(&buf[..ret as usize]);
        offset += ret;
    }
}

/// Copies `body` into memory from `alloc` and points `out` at
/// it (`NgxCopyRequestBody`). Returns false, leaving `out`
/// untouched, when the allocation fails.
///
/// # Safety
///
/// `alloc` must return either null or a pointer to `size`
/// writable bytes that stay valid for as long as `out` is
/// used; the caller must not read `out` past its length.
pub fn copy_request_body(
    body: &[u8],
    alloc: &dyn Fn(usize) -> *mut u8,
    out: &mut (*mut u8, usize),
) -> bool {
    let data = alloc(body.len());
    if data.is_null() {
        return false;
    }
    if !body.is_empty() {
        // SAFETY: `data` points at `body.len()` writable bytes by the
        // `alloc` contract, and `body` is a live shared slice.
        unsafe {
            std::ptr::copy_nonoverlapping(body.as_ptr(), data, body.len());
        }
    }
    *out = (data, body.len());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spooled body of `total` bytes of 'x', served from offsets.
    struct FakeBodyFile {
        total: i64,
        reads: usize,
    }

    impl FakeBodyFile {
        fn new(total: i64) -> FakeBodyFile {
            FakeBodyFile { total, reads: 0 }
        }

        fn reader(&mut self) -> impl FnMut(&mut [u8; 65536], i64) -> i64 + '_ {
            |buf, offset| {
                self.reads += 1;
                if offset < 0 || offset >= self.total {
                    return 0;
                }
                let n = (buf.len() as i64).min(self.total - offset);
                buf[..n as usize].fill(b'x');
                n
            }
        }
    }

    #[test]
    fn reads_small_body() {
        let mut file = FakeBodyFile::new(1000);
        let mut body = Vec::new();
        assert_eq!(
            RequestBodyStatus::Ok,
            read_request_body(&mut file.reader(), MAX_REQUEST_BODY_BYTES, &mut body)
        );
        assert_eq!(vec![b'x'; 1000], body);
    }

    #[test]
    fn reads_body_of_exactly_the_limit() {
        let mut file = FakeBodyFile::new(MAX_REQUEST_BODY_BYTES as i64);
        let mut body = Vec::new();
        assert_eq!(
            RequestBodyStatus::Ok,
            read_request_body(&mut file.reader(), MAX_REQUEST_BODY_BYTES, &mut body)
        );
        assert_eq!(MAX_REQUEST_BODY_BYTES, body.len());
    }

    #[test]
    fn oversized_body_is_rejected_with_bounded_work() {
        // 64 MiB on disk: the read stops shortly past the limit instead of
        // buffering the whole file into memory.
        let mut file = FakeBodyFile::new(64 << 20);
        let mut body = Vec::new();
        assert_eq!(
            RequestBodyStatus::TooLarge,
            read_request_body(&mut file.reader(), MAX_REQUEST_BODY_BYTES, &mut body)
        );
        assert!(file.reads <= 4);
        assert!(body.len() <= MAX_REQUEST_BODY_BYTES);
    }

    #[test]
    fn read_error_is_reported() {
        let mut read = |_buf: &mut [u8; 65536], _offset: i64| -1;
        let mut body = Vec::new();
        assert_eq!(
            RequestBodyStatus::ReadError,
            read_request_body(&mut read, MAX_REQUEST_BODY_BYTES, &mut body)
        );
    }

    #[test]
    fn copy_into_allocated_buffer() {
        let mut storage = [0u8; 16];
        let base = storage.as_mut_ptr();
        let mut out = (std::ptr::null_mut(), 0usize);
        assert!(copy_request_body(
            b"hello",
            &|n| if n <= 16 { base } else { std::ptr::null_mut() },
            &mut out
        ));
        assert_eq!(b"hello", unsafe {
            std::slice::from_raw_parts(out.0.cast_const(), out.1)
        });
        assert_eq!(storage.as_mut_ptr(), out.0);
    }

    #[test]
    fn failed_allocation_is_reported_not_written() {
        let sentinel = (0x1234 as *mut u8, 8usize);
        let mut out = sentinel;
        assert!(!copy_request_body(
            b"hello",
            &|_| std::ptr::null_mut(),
            &mut out
        ));
        assert_eq!(sentinel.0, out.0);
        assert_eq!(sentinel.1, out.1);
    }
}
