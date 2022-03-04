use super::{Event, Registration};
use futures::{channel::mpsc::UnboundedReceiver, Stream, StreamExt};
use mio::Interest;
use pin_project::pin_project;
use std::{
    io::{self, ErrorKind, Read, Write},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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
                // if reading would block the thread, continue to event polling
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                // if there was some other io error, bail
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
    writeable: Option<()>,

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
            writeable: Some(()),
            events,
        })
    }

    fn poll_event(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.as_mut().project();
        let e = this.events.poll_next(cx).ready()?;
        let e = e.ok_or_else(|| io::Error::new(ErrorKind::BrokenPipe, "channel disconnected"))?;

        *this.readable = this.readable.or_else(|| e.is_readable().then_some(()));
        *this.writeable = this.writeable.or_else(|| e.is_writable().then_some(()));
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
            // if the stream is readable
            if let Some(()) = self.readable.take() {
                // try read some bytes
                let b = buf.initialize_unfilled();
                match self.registration.read(b) {
                    Ok(n) => {
                        // if bytes were read, mark them
                        buf.advance(n);
                        // ensure that we attempt another read next time
                        // since no new readable events will come through
                        self.readable = Some(());
                        return Poll::Ready(Ok(()));
                    }
                    // if reading would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }
            self.as_mut().poll_event(cx)?.ready()?;
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
            if let Some(()) = self.writeable.take() {
                // try write some bytes
                match self.registration.write(buf) {
                    Ok(n) => {
                        // ensure that we attempt another write next time
                        // since no new writeable events will come through
                        self.writeable = Some(());
                        return Poll::Ready(Ok(n));
                    }
                    // if writing would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }

            self.as_mut().poll_event(cx)?.ready()?;
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        loop {
            // if the stream is writeable
            if let Some(()) = self.writeable.take() {
                // try flush the bytes
                match self.registration.flush() {
                    Ok(()) => {
                        // ensure that we attempt another write next time
                        // since no new writeable events will come through
                        self.writeable = Some(());
                        return Poll::Ready(Ok(()));
                    }
                    // if flushing would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }
            self.as_mut().poll_event(cx)?.ready()?;
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // shutdowns are immediate
        Poll::Ready(self.registration.shutdown(std::net::Shutdown::Write))
    }
}
