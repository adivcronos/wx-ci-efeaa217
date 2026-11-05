//! Byte counters for the live throughput readout.
//!
//! Every byte the app tunnels passes through the SSH local forward in
//! [`tunnel`](crate::tunnel) — in BOTH routing modes, since stealth/TUN also ends
//! up at the same local Xray → forward → server path. So the forward is the one
//! place worth counting, and [`Counted`] wraps the LOCAL end of each forwarded
//! connection: bytes read from it are upload, bytes written to it are download.
//!
//! The counters are process-global and monotonic; the FFI samples them on its
//! watchdog tick to derive a rate, and calls [`reset`] at the start of a session
//! so "this session" totals mean what they say.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

static UP_BYTES: AtomicU64 = AtomicU64::new(0);
static DOWN_BYTES: AtomicU64 = AtomicU64::new(0);

/// `(uploaded, downloaded)` bytes since the last [`reset`].
#[must_use]
pub fn totals() -> (u64, u64) {
    (
        UP_BYTES.load(Ordering::Relaxed),
        DOWN_BYTES.load(Ordering::Relaxed),
    )
}

/// Zero both counters (start of a session).
pub fn reset() {
    UP_BYTES.store(0, Ordering::Relaxed);
    DOWN_BYTES.store(0, Ordering::Relaxed);
}

/// Wraps the local end of a forwarded connection and tallies what flows through.
/// Reads (app → server) count as upload, writes (server → app) as download.
pub struct Counted<S> {
    inner: S,
}

impl<S> Counted<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            let n = buf.filled().len().saturating_sub(before);
            if n > 0 {
                UP_BYTES.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            DOWN_BYTES.fetch_add(*n as u64, Ordering::Relaxed);
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn counts_reads_as_upload_and_writes_as_download() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            reset();
            // A duplex pair stands in for the local app socket.
            let (a, b) = tokio::io::duplex(64);
            let mut counted = Counted::new(a);
            let mut peer = b;

            peer.write_all(b"hello").await.unwrap(); // app -> us -> server = upload
            let mut buf = [0u8; 5];
            counted.read_exact(&mut buf).await.unwrap();
            counted.write_all(b"1234567890").await.unwrap(); // server -> app = download

            let (up, down) = totals();
            assert_eq!(up, 5);
            assert_eq!(down, 10);
            reset();
        });
    }
}
