//! HTTPS/TLS stripping proxy functionality
//!
//! This module provides functionality to intercept HTTP connections on port 80
//! and transparently upgrade them to HTTPS connections on port 443.
//!
//! Supports SNI and will attempt to rewrite protocols

use anyhow::{Context, Result, bail};
use rustls::pki_types::ServerName;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Parse HTTP request headers to extract the Host header value
///
/// This function looks for the "Host:" header in the HTTP request.
/// It returns the hostname if found, or an error if the Host header is missing
/// or if the data doesn't look like an HTTP request.
pub fn extract_http_host(data: &[u8]) -> Result<String> {
    let request = std::str::from_utf8(data).context("Invalid UTF-8 in HTTP request")?;

    // Look for host header
    for line in request.lines() {
        let line = line.trim();
        if line.to_lowercase().starts_with("host:") {
            // Extract the hostname after "Host:"
            let host = line[5..].trim();
            // Strip port
            let host = if let Some(colon_pos) = host.find(':') {
                &host[..colon_pos]
            } else {
                host
            };
            return Ok(host.to_string());
        }
    }

    bail!("No Host header found in HTTP request")
}

const HTTPS_PATTERN: &[u8] = b"https://";

/// Since https to http means removing a character, we pad the replacement
/// with a space padded so things like Content-Length headers don't mess up
const HTTPS_REPLACEMENT: &[u8] = b" http://";

/// How long a held-back tail waits for more data before it is handed out as is
const TAIL_HOLD_LIMIT: Duration = Duration::from_millis(250);

/// Reader adapter that rewrites https:// to http:// in the data passing through it
///
/// A match can be split across reads, so a tail that could be the start of one is held
/// back until the next read shows whether it is. The rest of a match split at a TLS
/// record boundary may still be in flight, so the tail waits for it, but only for
/// `hold_limit`: the connection may be idle with the response complete (e.g. a
/// keep-alive response ending in "h"), and holding it for good would stall the client.
struct HttpsRewriter<R> {
    inner: R,
    /// Held-back tail: a proper prefix of the pattern, not yet rewritten or returned
    tail: Vec<u8>,
    /// When the current tail was read
    held_since: Option<Instant>,
    hold_limit: Duration,
    /// Rewritten data that did not fit in the caller's buffer
    pending: Vec<u8>,
}

impl<R: Read> HttpsRewriter<R> {
    fn new(inner: R) -> Self {
        Self::with_hold_limit(inner, TAIL_HOLD_LIMIT)
    }

    fn with_hold_limit(inner: R, hold_limit: Duration) -> Self {
        Self {
            inner,
            tail: Vec::with_capacity(HTTPS_PATTERN.len()),
            held_since: None,
            hold_limit,
            pending: Vec::new(),
        }
    }

    /// Rewrite https:// to http:// in a byte buffer
    fn rewrite_https_to_http(data: &mut [u8]) -> usize {
        let mut i = 0;
        let mut modifications = 0;

        while i + HTTPS_PATTERN.len() <= data.len() {
            if &data[i..i + HTTPS_PATTERN.len()] == HTTPS_PATTERN {
                data[i..i + HTTPS_REPLACEMENT.len()].copy_from_slice(HTTPS_REPLACEMENT);
                modifications += 1;
                i += HTTPS_REPLACEMENT.len();
            } else {
                i += 1;
            }
        }

        modifications
    }

    /// Length of the longest suffix of `data` that is a proper prefix of the pattern
    fn partial_match_len(data: &[u8]) -> usize {
        (1..HTTPS_PATTERN.len())
            .rev()
            .find(|&len| data.ends_with(&HTTPS_PATTERN[..len]))
            .unwrap_or(0)
    }

    /// Whether a read error means the held-back tail should be handed out now
    fn release_tail_on(&self, kind: std::io::ErrorKind) -> bool {
        match kind {
            std::io::ErrorKind::UnexpectedEof => true,
            std::io::ErrorKind::WouldBlock => self
                .held_since
                .is_some_and(|since| since.elapsed() >= self.hold_limit),
            _ => false,
        }
    }

    /// Read and rewrite into `buf`, which must be longer than the held-back tail.
    ///
    /// Returns 0 only at the end of the stream.
    fn fill(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let held = self.tail.len();
            buf[..held].copy_from_slice(&self.tail);
            self.tail.clear();

            let n = match self.inner.read(&mut buf[held..]) {
                Ok(n) => n,
                // The end of the stream (rustls reports a close without close_notify as
                // UnexpectedEof, on every read), or nothing more arrived within the hold
                // limit: hand out what was held back; the next read reports the condition.
                Err(e) if held > 0 && self.release_tail_on(e.kind()) => 0,
                Err(e) => {
                    self.tail.extend_from_slice(&buf[..held]);
                    return Err(e);
                }
            };
            if n == 0 {
                self.held_since = None;
                return Ok(held);
            }

            let total = held + n;
            Self::rewrite_https_to_http(&mut buf[..total]);
            let hold = Self::partial_match_len(&buf[..total]);
            self.tail.extend_from_slice(&buf[total - hold..total]);
            self.held_since = (hold > 0).then(Instant::now);
            if total > hold {
                return Ok(total - hold);
            }
        }
    }
}

impl<R: Read> Read for HttpsRewriter<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // fill() needs room for the held-back tail plus at least one new byte
        if self.pending.is_empty() && buf.len() < HTTPS_PATTERN.len() {
            let mut scratch = [0; HTTPS_PATTERN.len() * 2];
            let n = self.fill(&mut scratch)?;
            if n == 0 {
                return Ok(0);
            }
            self.pending.extend_from_slice(&scratch[..n]);
        }

        if !self.pending.is_empty() {
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            return Ok(n);
        }

        self.fill(buf)
    }
}

/// Wrapper around a TcpStream that performs HTTPS stripping
pub struct HttpsStrippingStream {
    /// The underlying TLS connection, with https:// rewritten on the way in
    tls_stream: HttpsRewriter<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>,
}

impl HttpsStrippingStream {
    /// Create a new HTTPS stripping stream by establishing a TLS connection
    ///
    /// # Arguments
    /// * `hostname` - The hostname to connect to (used for SNI)
    /// * `ip` - The IP address to connect to
    /// * `port` - The port to connect to
    pub fn connect(hostname: &str, ip: std::net::IpAddr, port: u16) -> Result<Self> {
        // Load native root certificates
        let mut root_store = rustls::RootCertStore::empty();
        let certs = rustls_native_certs::load_native_certs();
        for cert in certs.certs {
            root_store.add(cert).ok();
        }
        if let Some(err) = certs.errors.first() {
            log::warn!("Error loading some native certificates: {}", err);
        }

        // Create TLS client configuration
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let server_name = ServerName::try_from(hostname.to_string())
            .context("Invalid hostname for SNI")?
            .to_owned();

        // Connect to the remote server
        let addr = SocketAddr::new(ip, port);
        let tcp_stream =
            TcpStream::connect(addr).context(format!("Failed to connect to {}:{}", ip, port))?;

        // Establish TLS connection
        let client_conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .context("Failed to create TLS connection")?;
        let tls_stream = rustls::StreamOwned::new(client_conn, tcp_stream);

        Ok(Self {
            tls_stream: HttpsRewriter::new(tls_stream),
        })
    }

    /// Set the underlying TCP stream to non-blocking mode
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<()> {
        self.tls_stream
            .inner
            .sock
            .set_nonblocking(nonblocking)
            .context("Failed to set nonblocking mode")
    }
}

impl Read for HttpsStrippingStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.tls_stream.read(buf)
    }
}

impl Write for HttpsStrippingStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tls_stream.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.tls_stream.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_http_host() {
        let request = b"GET / HTTP/1.1\r\nHost: snowemu.com\r\n\r\n";
        assert_eq!(extract_http_host(request).unwrap(), "snowemu.com");

        let request = b"GET / HTTP/1.1\r\nHost: snowemu.com:80\r\n\r\n";
        assert_eq!(extract_http_host(request).unwrap(), "snowemu.com");

        let request = b"GET / HTTP/1.1\r\nhost: snowemu.com\r\n\r\n";
        assert_eq!(extract_http_host(request).unwrap(), "snowemu.com");
    }

    #[test]
    fn test_rewrite_https_to_http() {
        let mut data = b"Visit https://snowemu.com for more info".to_vec();
        let mods = HttpsRewriter::<&[u8]>::rewrite_https_to_http(&mut data);
        assert_eq!(mods, 1);
        assert_eq!(&data[..], b"Visit  http://snowemu.com for more info");

        let mut data = b"https://a.com and https://b.com".to_vec();
        let mods = HttpsRewriter::<&[u8]>::rewrite_https_to_http(&mut data);
        assert_eq!(mods, 2);
        assert_eq!(&data[..], b" http://a.com and  http://b.com");
    }

    /// Reader that hands out fixed chunks; `None` reports WouldBlock once
    struct ChunkedReader {
        chunks: std::collections::VecDeque<Option<Vec<u8>>>,
        end: Option<std::io::ErrorKind>,
    }

    impl ChunkedReader {
        fn new(chunks: Vec<Option<&[u8]>>, end: Option<std::io::ErrorKind>) -> Self {
            Self {
                // An empty chunk would read as the end of the stream
                chunks: chunks
                    .into_iter()
                    .filter(|c| c.is_none_or(|c| !c.is_empty()))
                    .map(|c| c.map(<[u8]>::to_vec))
                    .collect(),
                end,
            }
        }
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.chunks.pop_front() {
                Some(None) => Err(std::io::ErrorKind::WouldBlock.into()),
                Some(Some(mut chunk)) => {
                    let n = chunk.len().min(buf.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    if n < chunk.len() {
                        self.chunks.push_front(Some(chunk.split_off(n)));
                    }
                    Ok(n)
                }
                None => match self.end {
                    Some(kind) => Err(kind.into()),
                    None => Ok(0),
                },
            }
        }
    }

    /// Read everything with reads of `buf_len`, skipping WouldBlock; returns the data and
    /// the error that ended the stream, if any
    fn read_to_end<R: Read>(
        reader: &mut R,
        buf_len: usize,
    ) -> (Vec<u8>, Option<std::io::ErrorKind>) {
        let mut out = Vec::new();
        let mut buf = vec![0; buf_len];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => return (out, None),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return (out, Some(e.kind())),
            }
        }
    }

    #[test]
    fn test_rewrite_split_across_reads() {
        let input: &[u8] = b"<a href=\"https://snowemu.com/\">https://x</a> http https:/";
        let expected: &[u8] = b"<a href=\" http://snowemu.com/\"> http://x</a> http https:/";

        for split in 0..=input.len() {
            for buf_len in [1, 2, 7, 8, 9, 16, 4096] {
                let (a, b) = input.split_at(split);
                let mut reader =
                    HttpsRewriter::new(ChunkedReader::new(vec![Some(a), Some(b)], None));
                let (out, err) = read_to_end(&mut reader, buf_len);
                assert_eq!(out, expected, "split at {split}, reads of {buf_len}");
                assert_eq!(err, None);
            }
        }
    }

    #[test]
    fn test_rewrite_holds_back_partial_match() {
        let mut reader = HttpsRewriter::new(ChunkedReader::new(
            vec![Some(b"see http"), Some(b"s://x")],
            None,
        ));
        let mut buf = [0; 8];

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"see ");
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b" http://");
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"x");
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn test_rewrite_waits_for_rest_of_match() {
        let mut reader = HttpsRewriter::new(ChunkedReader::new(
            vec![Some(b"see http"), None, None, Some(b"s://x")],
            None,
        ));
        let mut buf = [0; 64];

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"see ");
        for _ in 0..2 {
            assert_eq!(
                reader.read(&mut buf).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b" http://x");
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn test_rewrite_releases_tail_when_idle() {
        let mut reader = HttpsRewriter::with_hold_limit(
            ChunkedReader::new(
                vec![Some(b"body ending in h"), None, Some(b"ttps://x")],
                None,
            ),
            Duration::ZERO,
        );
        let mut buf = [0; 64];

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"body ending in ");
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"h");
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ttps://x");
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn test_rewrite_flushes_held_back_tail_at_end() {
        for end in [None, Some(std::io::ErrorKind::UnexpectedEof)] {
            let mut reader = HttpsRewriter::new(ChunkedReader::new(vec![Some(b"a http")], end));
            let (out, err) = read_to_end(&mut reader, 64);
            assert_eq!(out, b"a http");
            assert_eq!(err, end);
        }
    }
}
