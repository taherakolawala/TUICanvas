//! Bounded-memory multipart body: signed fields, then file bytes, then closing boundary.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use tokio::io::{AsyncRead, ReadBuf};

pub struct UploadBody {
    prefix: Option<Bytes>,
    file: tokio::fs::File,
    remaining: u64,
    suffix: Option<Bytes>,
    left: u64,
}

impl UploadBody {
    pub fn new(
        prefix: Vec<u8>,
        file: tokio::fs::File,
        size: u64,
        suffix: Vec<u8>,
    ) -> io::Result<Self> {
        let left = size
            .checked_add(prefix.len() as u64)
            .and_then(|n| n.checked_add(suffix.len() as u64))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "upload too large"))?;
        Ok(Self {
            prefix: Some(prefix.into()),
            file,
            remaining: size,
            suffix: Some(suffix.into()),
            left,
        })
    }

    pub fn len(&self) -> u64 {
        self.left
    }
}

impl Body for UploadBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if let Some(prefix) = self.prefix.take() {
            self.left -= prefix.len() as u64;
            return Poll::Ready(Some(Ok(Frame::data(prefix))));
        }
        if self.remaining > 0 {
            let mut buffer = vec![0u8; usize::try_from(self.remaining.min(64 * 1024)).unwrap()];
            let mut read = ReadBuf::new(&mut buffer);
            match Pin::new(&mut self.file).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Ok(())) => {
                    let n = read.filled().len();
                    if n == 0 {
                        return Poll::Ready(Some(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "submission file changed during upload",
                        ))));
                    }
                    buffer.truncate(n);
                    self.remaining -= n as u64;
                    self.left -= n as u64;
                    return Poll::Ready(Some(Ok(Frame::data(Bytes::from(buffer)))));
                }
            }
        }
        if let Some(suffix) = self.suffix.take() {
            self.left -= suffix.len() as u64;
            return Poll::Ready(Some(Ok(Frame::data(suffix))));
        }
        Poll::Ready(None)
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::poll_fn;

    #[tokio::test]
    async fn streams_all_parts_in_order_with_exact_length() {
        let path =
            std::env::temp_dir().join(format!("tuicanvas-upload-body-{}", std::process::id()));
        std::fs::write(&path, b"file bytes").unwrap();
        let file = tokio::fs::File::open(&path).await.unwrap();
        let mut body = UploadBody::new(b"prefix".to_vec(), file, 10, b"suffix".to_vec()).unwrap();
        assert_eq!(body.len(), 22);
        let mut all = Vec::new();
        while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
            all.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        assert_eq!(all, b"prefixfile bytessuffix");
        assert_eq!(body.len(), 0);
        std::fs::remove_file(path).unwrap();
    }
}
