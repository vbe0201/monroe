use std::fmt;

/// Error produced by the [`Sender`] when trying to send a value
/// to an already dropped [`Receiver`].
///
/// [`Sender`]: super::Sender
/// [`Receiver`]: super::Receiver
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T> SendError<T> {
    /// Consumes the error, yielding the message that failed to
    /// be sent.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "sending to a closed channel".fmt(f)
    }
}

impl<T: fmt::Debug> std::error::Error for SendError<T> {}

/// Error produced by the [`Sender`] when trying to send a value
/// to an already dropped or full [`Receiver`].
///
/// [`Sender`]: super::Sender
/// [`Receiver`]: super::Receiver
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrySendError<T> {
    /// The channel's buffer capacity for messages was exhausted.
    Full(T),
    /// The [`Receiver`] was already dropped.
    /// 
    /// [`Receiver`]: super::Receiver
    Disconnected(T),
}

impl<T> fmt::Display for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TrySendError::*;
        match self {
            Full(_) => f.write_str("sending to a full channel"),
            Disconnected(_) => f.write_str("sending ro a closed channel"),
        }
    }
}

impl<T: fmt::Debug> std::error::Error for TrySendError<T> {}

impl<T> From<SendError<T>> for TrySendError<T> {
    fn from(SendError(msg): SendError<T>) -> Self {
        TrySendError::Disconnected(msg)
    }
}

/// Error produced by the [`Receiver`] when waiting for messages
/// from already dropped [`Sender`]s.
///
/// [`Receiver`]: super::Receiver
/// [`Sender`]: super::Sender
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "receiving from a closed channel".fmt(f)
    }
}

impl std::error::Error for RecvError {}

/// Error produced by the receiver when trying to wait for a value
/// fails due to all senders being disconnected.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TryRecvError {
    //// The channel is currently empty, but [`Sender`]s still
    /// exist.
    ///
    /// [`Sender`]: super::Sender
    Empty,
    /// The channel is permanently disconnected and no new messages
    /// may be received.
    Disconnected,
}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TryRecvError::*;
        match self {
            Empty => "receiving from an empty channel".fmt(f),
            Disconnected => "receiving from a closed channel".fmt(f),
        }
    }
}

impl std::error::Error for TryRecvError {}

impl From<RecvError> for TryRecvError {
    fn from(_: RecvError) -> Self {
        TryRecvError::Disconnected
    }
}
