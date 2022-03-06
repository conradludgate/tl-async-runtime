use std::{
    io::{self, Read, Write},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{channel::mpsc::UnboundedReceiver, Stream, StreamExt};
use mio::Interest;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{Event, Registration};

/// Listener for TCP events
pub struct TcpListener {
    registration: Registration<mio::net::TcpListener>,
}

impl TcpListener {
    /// Create a new TcpListener bound to the socket
    pub fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = mio::net::TcpListener::bind(addr)?;
        let registration = Registration::new(listener, Interest::READABLE)?;
        Ok(Self { registration })
    }

    /// Accept a new TcpStream to communicate with
    pub async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        loop {
            self.registration.events().next().await;
            match self.registration.accept() {
                Ok((stream, socket)) => break Ok((TcpStream::from_mio(stream)?, socket)),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => break Err(e),
            }
        }
    }
}

/// Handles communication over a TCP connection
#[pin_project]
pub struct TcpStream {
    registration: Registration<mio::net::TcpStream>,

    readable: Option<()>,
    writable: Option<()>,
    read_closed: bool,

    #[pin]
    events: UnboundedReceiver<Event>,
}

impl TcpStream {
    pub(crate) fn from_mio(stream: mio::net::TcpStream) -> std::io::Result<Self> {
        // register the stream to the OS
        let registration = Registration::new(stream, Interest::READABLE | Interest::WRITABLE)?;
        let events = registration.events();
        Ok(Self {
            registration,
            readable: Some(()),
            writable: Some(()),
            read_closed: false,
            events,
        })
    }

    fn poll_event(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut this = self.as_mut().project();
        let event = match this.events.as_mut().poll_next(cx) {
            Poll::Ready(Some(event)) => event,
            Poll::Ready(None) => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel disconnected",
                )))
            }
            Poll::Pending => return Poll::Pending,
        };

        if event.is_readable() {
            *this.readable = Some(());
        }
        if event.is_writable() {
            *this.writable = Some(());
        }
        *this.read_closed |= event.is_read_closed();
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if self.read_closed {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF")));
            }
            // if the stream is readable
            if let Some(()) = self.readable.take() {
                // try read some bytes
                let b = buf.initialize_unfilled();
                match self.registration.read(b) {
                    Ok(n) => {
                        // eprintln!("{:?}", String::from_utf8_lossy(&b[..n]));
                        // if bytes were read, mark them
                        buf.advance(n);
                        // ensure that we attempt another read next time
                        // since no new readable events will come through
                        // https://docs.rs/mio/0.8.0/mio/struct.Poll.html#draining-readiness
                        self.readable = Some(());
                        return Poll::Ready(Ok(()));
                    }
                    // if reading would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }

            match self.as_mut().poll_event(cx)? {
                Poll::Ready(()) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        loop {
            // if the stream is writeable
            if let Some(()) = self.writable.take() {
                // try write some bytes
                match self.registration.write(buf) {
                    Ok(n) => {
                        // ensure that we attempt another write next time
                        // since no new writeable events will come through
                        self.writable = Some(());
                        return Poll::Ready(Ok(n));
                    }
                    // if writing would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }

            match self.as_mut().poll_event(cx)? {
                Poll::Ready(()) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        loop {
            // if the stream is writeable
            if let Some(()) = self.writable.take() {
                // try flush the bytes
                match self.registration.flush() {
                    Ok(()) => {
                        // ensure that we attempt another write next time
                        // since no new writeable events will come through
                        self.writable = Some(());
                        return Poll::Ready(Ok(()));
                    }
                    // if flushing would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }

            match self.as_mut().poll_event(cx)? {
                Poll::Ready(()) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // shutdowns are immediate
        Poll::Ready(self.registration.shutdown(std::net::Shutdown::Write))
    }
}
