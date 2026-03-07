//! WebSocket client for connecting to a flighthook server.
//!
//! Requires the `client` feature flag. Provides a synchronous, caller-driven
//! API (no async runtime dependency) with both blocking and non-blocking
//! receive modes.
//!
//! # Blocking
//!
//! ```no_run
//! use flighthook::FlighthookClient;
//!
//! let mut client = FlighthookClient::connect("ws://localhost:3030/api/ws", "my-app").unwrap();
//! loop {
//!     match client.recv() {
//!         Ok(msg) => println!("{}: {:?}", msg.source, msg.event),
//!         Err(e) => { eprintln!("{e}"); break; }
//!     }
//! }
//! ```
//!
//! # Game loop (non-blocking)
//!
//! ```no_run
//! use flighthook::FlighthookClient;
//!
//! let mut client = FlighthookClient::connect("ws://localhost:3030/api/ws", "my-sim").unwrap();
//! client.set_nonblocking(true).unwrap();
//!
//! loop {
//!     // Drain all pending messages
//!     while let Ok(Some(msg)) = client.try_recv() {
//!         println!("{}: {:?}", msg.source, msg.event);
//!     }
//!     // ... render frame, sleep, etc.
//! }
//! ```

use std::fmt;
use std::net::TcpStream;

use tungstenite::protocol::WebSocket;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Message;

use crate::FlighthookMessage;

/// Errors returned by [`FlighthookClient`] operations.
#[derive(Debug)]
pub enum ClientError {
    /// WebSocket connection or I/O error.
    WebSocket(Box<tungstenite::Error>),
    /// JSON serialization/deserialization error.
    Json(serde_json::Error),
    /// The connection was closed.
    Closed,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::WebSocket(e) => write!(f, "websocket: {e}"),
            ClientError::Json(e) => write!(f, "json: {e}"),
            ClientError::Closed => write!(f, "connection closed"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::WebSocket(e) => Some(e.as_ref()),
            ClientError::Json(e) => Some(e),
            ClientError::Closed => None,
        }
    }
}

impl From<tungstenite::Error> for ClientError {
    fn from(e: tungstenite::Error) -> Self {
        ClientError::WebSocket(Box::new(e))
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Json(e)
    }
}

/// A synchronous WebSocket client connected to a flighthook server.
///
/// After [`connect`](Self::connect), the handshake is complete and the client
/// is ready to receive and send [`FlighthookMessage`] events.
///
/// Defaults to blocking mode. Call [`set_nonblocking(true)`](Self::set_nonblocking)
/// for game-loop integration, then use [`try_recv`](Self::try_recv) to poll.
pub struct FlighthookClient {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    source_id: String,
}

impl FlighthookClient {
    /// Connect to a flighthook server and complete the init handshake.
    ///
    /// `url` should be a WebSocket URL like `"ws://localhost:3030/api/ws"`.
    /// `name` is a human-readable client identifier sent during the handshake.
    pub fn connect(url: &str, name: &str) -> Result<Self, ClientError> {
        let (mut socket, _response) = tungstenite::connect(url)?;

        let start = serde_json::json!({ "type": "start", "name": name });
        socket.send(Message::text(start.to_string()))?;

        // Wait for init response (blocking — handshake always blocks)
        let source_id = loop {
            match socket.read()? {
                Message::Text(text) => match parse_init_source_id(&text) {
                    Some(id) => break id,
                    None => continue,
                },
                Message::Close(_) => return Err(ClientError::Closed),
                _ => continue,
            }
        };

        Ok(Self { socket, source_id })
    }

    /// The unique source ID assigned by the server (e.g. `"ws.a1b2c3d4"`).
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Set the underlying TCP stream to non-blocking mode.
    ///
    /// When non-blocking, [`try_recv`](Self::try_recv) returns `Ok(None)`
    /// immediately if no data is available. [`recv`](Self::recv) should not
    /// be used in non-blocking mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), ClientError> {
        match self.socket.get_ref() {
            MaybeTlsStream::Plain(tcp) => tcp
                .set_nonblocking(nonblocking)
                .map_err(|e| ClientError::WebSocket(Box::new(tungstenite::Error::Io(e)))),
            _ => Ok(()),
        }
    }

    /// Block until the next [`FlighthookMessage`] arrives.
    ///
    /// Use this in blocking mode (the default). For game-loop integration,
    /// use [`set_nonblocking`](Self::set_nonblocking) + [`try_recv`](Self::try_recv).
    pub fn recv(&mut self) -> Result<FlighthookMessage, ClientError> {
        loop {
            match self.socket.read()? {
                Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                Message::Close(_) => return Err(ClientError::Closed),
                _ => continue,
            }
        }
    }

    /// Poll for the next message without blocking.
    ///
    /// Returns `Ok(None)` when no message is immediately available (requires
    /// [`set_nonblocking(true)`](Self::set_nonblocking)). Call in a loop to
    /// drain all pending messages each frame.
    pub fn try_recv(&mut self) -> Result<Option<FlighthookMessage>, ClientError> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Ok(Some(serde_json::from_str(&text)?)),
                Ok(Message::Close(_)) => return Err(ClientError::Closed),
                Ok(_) => continue,
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Send a [`FlighthookMessage`] to the server.
    pub fn send(&mut self, msg: &FlighthookMessage) -> Result<(), ClientError> {
        let json = serde_json::to_string(msg)?;
        self.socket.send(Message::text(json))?;
        Ok(())
    }

    /// Cleanly close the WebSocket connection.
    pub fn close(mut self) -> Result<(), ClientError> {
        self.socket.close(None)?;
        loop {
            match self.socket.read() {
                Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                    return Ok(());
                }
                Err(tungstenite::Error::AlreadyClosed) => return Ok(()),
                Err(e) => return Err(e.into()),
                _ => continue,
            }
        }
    }
}

impl fmt::Debug for FlighthookClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlighthookClient")
            .field("source_id", &self.source_id)
            .finish_non_exhaustive()
    }
}

fn parse_init_source_id(text: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct InitMsg {
        #[serde(rename = "type")]
        msg_type: String,
        source_id: String,
    }
    let msg: InitMsg = serde_json::from_str(text).ok()?;
    (msg.msg_type == "init").then_some(msg.source_id)
}
