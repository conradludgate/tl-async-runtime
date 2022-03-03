use std::{
    io::{self, Read, Write},
    mem::MaybeUninit,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{channel::mpsc::UnboundedReceiver, Stream, StreamExt};
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{Event, Registration};

pub struct TcpListener {
    registration: Registration<mio::net::TcpListener>,
}

impl TcpListener {
    pub fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = mio::net::TcpListener::bind(addr)?;
        let registration = super::Registration::new(listener, mio::Interest::READABLE)?;
        Ok(Self { registration })
    }

    pub async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        loop {
            self.registration.ready().next().await;
            match self.registration.accept() {
                Ok((stream, socket)) => break Ok((TcpStream::from_mio(stream)?, socket)),
                Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => break Err(e),
                _ => {}
            }
        }
    }
}

#[pin_project]
pub struct TcpStream {
    registration: Registration<mio::net::TcpStream>,
    readable: Option<()>,
    writeable: Option<()>,

    #[pin]
    ready: UnboundedReceiver<Event>,
}

impl TcpStream {
    pub(crate) fn from_mio(stream: mio::net::TcpStream) -> std::io::Result<Self> {
        let registration =
            super::Registration::new(stream, mio::Interest::READABLE | mio::Interest::WRITABLE)?;
        let ready = registration.ready();
        Ok(Self {
            registration,
            readable: Some(()),
            writeable: Some(()),
            ready,
        })
    }

    fn poll_event(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut this = self.as_mut().project();
        let event = match this.ready.as_mut().poll_next(cx) {
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
            *this.writeable = Some(());
        }
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
            if let Some(()) = self.readable.take() {
                unsafe {
                    let b = slice_assume_init_mut(buf.unfilled_mut());
                    match self.registration.read(b) {
                        Ok(n) => {
                            buf.assume_init(n);
                            buf.advance(n);
                            self.readable = Some(());
                            return Poll::Ready(Ok(()));
                        }
                        Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => {
                            return Poll::Ready(Err(e))
                        }
                        _ => {}
                    }
                }
            }

            match self.as_mut().poll_event(cx) {
                Poll::Ready(_) => {}
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
            if let Some(()) = self.writeable.take() {
                match self.registration.write(buf) {
                    Ok(n) => {
                        self.writeable = Some(());
                        return Poll::Ready(Ok(n));
                    }
                    Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => {
                        return Poll::Ready(Err(e))
                    }
                    _ => {}
                }
            }
            // self.registration.reregister(mio::Interest::WRITABLE)?;

            match self.as_mut().poll_event(cx) {
                Poll::Ready(_) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        loop {
            if let Some(()) = self.writeable.take() {
                match self.registration.flush() {
                    Ok(n) => return Poll::Ready(Ok(n)),
                    Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => {
                        return Poll::Ready(Err(e))
                    }
                    _ => {}
                }
            }

            match self.as_mut().poll_event(cx) {
                Poll::Ready(_) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// TODO: This could use `MaybeUninit::slice_assume_init_mut` when it is stable.
unsafe fn slice_assume_init_mut(slice: &mut [MaybeUninit<u8>]) -> &mut [u8] {
    &mut *(slice as *mut [MaybeUninit<u8>] as *mut [u8])
}
