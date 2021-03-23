use crate::Body;
use bytes::Bytes;
use std::{
    io::{self, Read},
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::{
    mpsc::{channel, error::SendError, Receiver},
    oneshot,
};

/// Adapter that converts an [`io::Read`] into a [`Body`].
///
/// As `io::Read` is free to block, `ReadBody` will spawn a blocking task where
/// all the reading is done.
///
/// [`io::Read`]: std::io::Read
#[derive(Debug)]
pub struct ReadBody<R> {
    rx: Receiver<Result<Bytes, io::Error>>,
    start_tx: Option<oneshot::Sender<()>>,
    _marker: PhantomData<R>,
}

impl<R> ReadBody<R> {
    /// Create a new `ReadBody`.
    pub fn new(mut read: R) -> Self
    where
        R: Read + Send + Unpin + 'static,
    {
        let (tx, rx) = channel(10);
        let (start_tx, start_rx) = oneshot::channel();

        tokio::task::spawn(async move {
            // wait for someone to call `poll_data` to start reading
            if start_rx.await.is_err() {
                return;
            }

            let _ = tokio::task::spawn_blocking(move || {
                let mut buf = [0; 1024];

                loop {
                    if tx.is_closed() {
                        // fine to ignore the receiver going away
                        return;
                    }

                    let result = match read.read(&mut buf) {
                        Ok(bytes_read) if bytes_read == 0 => return,
                        Ok(bytes_read) => {
                            let bytes = Bytes::copy_from_slice(&buf[..bytes_read]);
                            tx.blocking_send(Ok(bytes))
                        }
                        Err(err) => tx.blocking_send(Err(err)),
                    };

                    if matches!(result, Err(SendError { .. })) {
                        // fine to ignore send errors due to receiver having
                        // gone away
                        return;
                    }
                }
            })
            .await;
        });

        ReadBody {
            rx,
            start_tx: Some(start_tx),
            _marker: PhantomData,
        }
    }
}

impl<R> Body for ReadBody<R>
where
    R: Read + Unpin,
{
    type Data = Bytes;
    type Error = io::Error;

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        // tell the worker to start reading
        if let Some(start_tx) = self.start_tx.take() {
            if start_tx.send(()).is_err() {
                let err = io::Error::new(io::ErrorKind::Other, "background worker is gone");
                return Poll::Ready(Some(Err(err)));
            }
        }

        self.rx.poll_recv(cx)
    }

    #[inline]
    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        Poll::Ready(Ok(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[tokio::test]
    async fn works() {
        let read = std::fs::File::open("Cargo.toml").unwrap();
        let body = ReadBody::new(read);

        let bytes = to_bytes(body).await.unwrap();
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(s.contains("name = \"http-body\""));
    }

    async fn to_bytes<B>(mut body: B) -> Result<Bytes, B::Error>
    where
        B: Body<Data = Bytes> + Unpin,
    {
        let mut buf = BytesMut::new();

        loop {
            let chunk = body.data().await.transpose()?;

            if let Some(chunk) = chunk {
                buf.extend(&chunk[..]);
            } else {
                return Ok(buf.freeze());
            }
        }
    }
}
