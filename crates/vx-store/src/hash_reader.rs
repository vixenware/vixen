//! Hash-verifying and counting AsyncRead wrappers
//!
//! - [`Sha256VerifyingReader`]: Streams data through while computing a SHA256 hash, verifying on EOF.
//! - [`CountingReader`]: Tracks total bytes read through an atomic counter for progress reporting.

use pin_project_lite::pin_project;
use sha2::{Digest, Sha256};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

pin_project! {
    /// AsyncRead wrapper that verifies SHA256 hash on completion.
    ///
    /// Computes the hash while streaming data through, then verifies
    /// when EOF is reached. Returns an error if the hash doesn't match.
    pub struct Sha256VerifyingReader<R> {
        #[pin]
        inner: R,
        hasher: Sha256,
        expected: String,
        verified: bool,
    }
}

impl<R: AsyncRead> Sha256VerifyingReader<R> {
    /// Create a new verifying reader with an expected SHA256 hex string.
    pub fn new(inner: R, expected_sha256: String) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            expected: expected_sha256,
            verified: false,
        }
    }
}

impl<R: AsyncRead> AsyncRead for Sha256VerifyingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.project();
        let before = buf.filled().len();

        match this.inner.poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let after = buf.filled().len();
                let new_data = &buf.filled()[before..after];

                // Update hash with new data
                this.hasher.update(new_data);

                // If we hit EOF (no new data), verify the hash
                if new_data.is_empty() && !*this.verified {
                    let actual = hex::encode(this.hasher.finalize_reset());
                    if actual.to_lowercase() != this.expected.to_lowercase() {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "SHA256 mismatch: expected {}, got {}",
                                this.expected, actual
                            ),
                        )));
                    }
                    *this.verified = true;
                }

                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pin_project! {
    /// AsyncRead wrapper that counts total bytes read through a shared atomic counter.
    ///
    /// Enables progress reporting from outside the reader chain without requiring
    /// async calls inside `poll_read`.
    pub struct CountingReader<R> {
        #[pin]
        inner: R,
        bytes_read: Arc<AtomicU64>,
    }
}

impl<R> CountingReader<R> {
    pub fn new(inner: R, counter: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            bytes_read: counter,
        }
    }
}

impl<R: AsyncRead> AsyncRead for CountingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.project();
        let before = buf.filled().len();
        match this.inner.poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let new_bytes = (buf.filled().len() - before) as u64;
                this.bytes_read.fetch_add(new_bytes, Ordering::Relaxed);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
