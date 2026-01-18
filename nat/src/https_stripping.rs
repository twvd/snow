//! HTTPS/TLS stripping proxy functionality
//!
//! This module provides functionality to intercept HTTP connections on port 80
//! and transparently upgrade them to HTTPS connections on port 443.
//!
//! Supports SNI and will attempt to rewrite protocols

use anyhow::{bail, Context, Result};
use rustls::pki_types::ServerName;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

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

/// Wrapper around a TcpStream that performs HTTPS stripping
pub struct HttpsStrippingStream {
    /// The underlying TLS connection
    tls_stream: rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
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

        Ok(Self { tls_stream })
    }

    /// Set the underlying TCP stream to non-blocking mode
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<()> {
        self.tls_stream
            .sock
            .set_nonblocking(nonblocking)
            .context("Failed to set nonblocking mode")
    }

    /// Rewrite https:// to http:// in a byte buffer
    /// Since https to http means removing a character, we pad the replacement
    /// with a space padded so things like Content-Length headers don't mess up
    fn rewrite_https_to_http(data: &mut [u8]) -> usize {
        let pattern = b"https://";
        let replacement = b" http://";

        let mut i = 0;
        let mut modifications = 0;

        while i + pattern.len() <= data.len() {
            if &data[i..i + pattern.len()] == pattern {
                data[i..i + replacement.len()].copy_from_slice(replacement);
                modifications += 1;
                i += replacement.len();
            } else {
                i += 1;
            }
        }

        modifications
    }
}

impl Read for HttpsStrippingStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.tls_stream.read(buf)?;
        if n > 0 {
            // TODO this breaks if the needle spans more than one read
            Self::rewrite_https_to_http(&mut buf[..n]);
        }
        Ok(n)
    }
}

impl Write for HttpsStrippingStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tls_stream.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.tls_stream.flush()
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
        let mods = HttpsStrippingStream::rewrite_https_to_http(&mut data);
        assert_eq!(mods, 1);
        assert_eq!(&data[..], b"Visit  http://snowemu.com for more info");

        let mut data = b"https://a.com and https://b.com".to_vec();
        let mods = HttpsStrippingStream::rewrite_https_to_http(&mut data);
        assert_eq!(mods, 2);
        assert_eq!(&data[..], b" http://a.com and  http://b.com");
    }
}
