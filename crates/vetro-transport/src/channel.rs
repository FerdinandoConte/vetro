//! Typed, length-prefixed message streams (ARCHITECTURE section 7.1).

use crate::{Error, close_code};
use core::marker::PhantomData;
use vetro_protocol::messages::{StreamMessage, decode_framed, encode_framed};

/// Sending half of one stream; one message type per stream.
pub struct MsgSender<M> {
    stream: quinn::SendStream,
    buf: Vec<u8>,
    _msg: PhantomData<fn(M)>,
}

impl<M: StreamMessage> MsgSender<M> {
    pub(crate) fn new(stream: quinn::SendStream, priority: i32) -> Self {
        // Fails only on a stream that is already closed; the first send then reports it.
        let _ = stream.set_priority(priority);
        Self {
            stream,
            buf: Vec::new(),
            _msg: PhantomData,
        }
    }

    /// Validates, frames and queues `msg`. The buffer is reused, so steady state allocates
    /// nothing here.
    pub async fn send(&mut self, msg: &M) -> Result<(), Error> {
        self.buf.clear();
        encode_framed(msg, &mut self.buf)?;
        self.stream.write_all(&self.buf).await?;
        Ok(())
    }

    /// Ends the stream cleanly after everything queued.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.stream
            .finish()
            .map_err(|_| Error::Stream("stream already closed"))
    }

    /// The scheduling priority of this stream (higher is sent first).
    pub fn priority(&self) -> Option<i32> {
        self.stream.priority().ok()
    }
}

/// Receiving half of one stream. Buffers at most one message (`4 + M::MAX_BYTES` bytes).
pub struct MsgReceiver<M> {
    stream: quinn::RecvStream,
    buf: Vec<u8>,
    _msg: PhantomData<fn() -> M>,
}

impl<M: StreamMessage> MsgReceiver<M> {
    pub(crate) fn new(stream: quinn::RecvStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            _msg: PhantomData,
        }
    }

    /// The next message, or `None` when the peer finished the stream cleanly. A protocol
    /// violation stops the stream and returns `Err`; the caller should close the session.
    pub async fn recv(&mut self) -> Result<Option<M>, Error> {
        loop {
            match decode_framed::<M>(&self.buf) {
                Ok(Some((msg, used))) => {
                    self.buf.drain(..used);
                    return Ok(Some(msg));
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = self.stream.stop(close_code::PROTOCOL_ERROR.into());
                    return Err(Error::Decode(e));
                }
            }
            // decode_framed checked the length prefix, so the pending message ends within
            // 4 + MAX_BYTES: never read past it.
            let room = (4 + M::MAX_BYTES).saturating_sub(self.buf.len()).max(1);
            match self.stream.read_chunk(room, true).await? {
                Some(chunk) => self.buf.extend_from_slice(&chunk.bytes),
                None if self.buf.is_empty() => return Ok(None),
                None => return Err(Error::Protocol("stream ended inside a message")),
            }
        }
    }
}
