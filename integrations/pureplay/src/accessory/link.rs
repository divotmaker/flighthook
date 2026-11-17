//! The card's link to Pureplay: [`Link`], one connection that sends and
//! receives frames, and [`WsLink`], its WebSocket form.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::{HandshakeError, Message, WebSocket};

/// Whether the card sends its JSON in binary frames rather than text ones.
/// Pureplay's example clients send text.
pub const SEND_BINARY: bool = false;

/// One frame received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A text frame.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
    /// The peer closed the connection, with its close code and reason.
    Close { code: Option<u16>, reason: String },
    /// A frame that carries no message (a ping or a pong); the link is
    /// alive.
    Control,
}

/// One connection to Pureplay. Non-blocking in the sense that a receive
/// waits for at most the stream's own poll.
pub trait Link {
    /// Send `text` as one frame.
    ///
    /// # Errors
    ///
    /// Returns the I/O error that ended the write.
    fn send(&mut self, text: &str) -> io::Result<()>;

    /// The next frame, or `None` when nothing arrived within the poll.
    ///
    /// # Errors
    ///
    /// Returns the I/O error that ended the connection.
    fn recv(&mut self) -> io::Result<Option<Frame>>;

    /// Send a ping.
    ///
    /// # Errors
    ///
    /// Returns the I/O error that ended the write.
    fn ping(&mut self) -> io::Result<()>;

    /// Close the connection from this side.
    fn close(&mut self);
}

/// Why [`WsLink::open`] gave no link.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The address makes no WebSocket request.
    #[error("WebSocket request: {0}")]
    Request(String),
    /// The handshake failed.
    #[error("WebSocket handshake failed: {0}")]
    Handshake(String),
    /// Pureplay did not answer the handshake in time.
    #[error("no WebSocket handshake reply")]
    NoReply,
    /// The caller asked to stop while the handshake waited.
    #[error("stopped")]
    Stopped,
}

/// The card's TCP stream. A read that times out reads as `WouldBlock` on
/// every platform (Windows reports `TimedOut`), so the WebSocket handshake
/// can be resumed after each poll.
struct Polled(TcpStream);

impl io::Read for Polled {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf).map_err(|e| {
            if e.kind() == io::ErrorKind::TimedOut {
                io::ErrorKind::WouldBlock.into()
            } else {
                e
            }
        })
    }
}

impl io::Write for Polled {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// One WebSocket connection (subprotocol `binary`).
pub struct WsLink {
    ws: WebSocket<Polled>,
}

impl WsLink {
    /// Run the client handshake with Pureplay at `addr` over `stream`, whose
    /// read timeout is the poll. Each poll without an answer asks
    /// `keep_going`; `false` ends the wait with [`OpenError::Stopped`]. No
    /// answer within `timeout` is [`OpenError::NoReply`].
    ///
    /// # Errors
    ///
    /// Returns an [`OpenError`].
    pub fn open(
        stream: TcpStream,
        addr: SocketAddr,
        timeout: Duration,
        mut keep_going: impl FnMut() -> bool,
    ) -> Result<Self, OpenError> {
        let mut request = format!("ws://{addr}/")
            .into_client_request()
            .map_err(|e| OpenError::Request(e.to_string()))?;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            tungstenite::http::HeaderValue::from_static("binary"),
        );
        let deadline = Instant::now() + timeout;
        let mut attempt = tungstenite::client(request, Polled(stream));
        loop {
            match attempt {
                Ok((ws, _)) => return Ok(Self { ws }),
                Err(HandshakeError::Failure(e)) => return Err(OpenError::Handshake(e.to_string())),
                Err(HandshakeError::Interrupted(mid)) => {
                    if !keep_going() {
                        return Err(OpenError::Stopped);
                    }
                    if Instant::now() >= deadline {
                        return Err(OpenError::NoReply);
                    }
                    attempt = mid.handshake();
                }
            }
        }
    }
}

fn io_error(e: tungstenite::Error) -> io::Error {
    match e {
        tungstenite::Error::Io(e) => e,
        e => io::Error::other(e),
    }
}

impl Link for WsLink {
    fn send(&mut self, text: &str) -> io::Result<()> {
        let msg = if SEND_BINARY {
            Message::binary(text.as_bytes().to_vec())
        } else {
            Message::text(text.to_owned())
        };
        self.ws.send(msg).map_err(io_error)
    }

    fn recv(&mut self) -> io::Result<Option<Frame>> {
        let msg = match self.ws.read() {
            Ok(msg) => msg,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(io_error(e)),
        };
        Ok(Some(match msg {
            Message::Text(t) => Frame::Text(t.as_str().to_owned()),
            Message::Binary(b) => Frame::Binary(b.to_vec()),
            Message::Close(frame) => {
                let _ = self.ws.flush();
                Frame::Close {
                    code: frame.as_ref().map(|f| u16::from(f.code)),
                    reason: frame.map_or_else(String::new, |f| f.reason.as_str().to_owned()),
                }
            }
            _ => Frame::Control,
        }))
    }

    fn ping(&mut self) -> io::Result<()> {
        self.ws
            .send(Message::Ping(tungstenite::Bytes::new()))
            .map_err(io_error)
    }

    fn close(&mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }
}
