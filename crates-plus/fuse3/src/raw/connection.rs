//! FUSE connection: tokio-only, Linux-only.
//!
//! For unprivileged mounts, uses fusermount3 + non-blocking AsyncFd.
//! For privileged mounts, uses blocking /dev/fuse + spawn_blocking.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io;
use std::io::{IoSlice, IoSliceMut, Write};
use std::ops::Deref;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_notify::Notify;
use futures_util::lock::Mutex;
use futures_util::{FutureExt, select};
use moire::process::Command;
use nix::fcntl::{FcntlArg, FdFlag, OFlag};
use nix::sys::socket::{self, AddressFamily, ControlMessageOwned, MsgFlags, SockFlag, SockType};
use nix::sys::uio;
use std::pin::pin;
use tokio::io::unix::AsyncFd;
use tokio::task;
use tracing::debug;

use crate::MountOptions;

pub(crate) type CompleteIoResult<T, U> = (T, io::Result<U>);

fn find_fusermount3() -> io::Result<std::path::PathBuf> {
    which::which("fusermount3")
        .map_err(|err| io::Error::other(format!("find fusermount3 binary failed {err:?}")))
}

#[derive(Debug)]
pub struct FuseConnection {
    unmount_notify: Arc<Notify>,
    mode: ConnectionMode,
}

#[derive(Debug)]
enum ConnectionMode {
    Block(BlockFuseConnection),
    NonBlock(NonBlockFuseConnection),
}

impl FuseConnection {
    pub fn new(unmount_notify: Arc<Notify>) -> io::Result<Self> {
        let connection = BlockFuseConnection::new()?;
        Ok(Self {
            unmount_notify,
            mode: ConnectionMode::Block(connection),
        })
    }

    pub async fn new_with_unprivileged(
        mount_options: MountOptions,
        mount_path: impl AsRef<Path>,
        unmount_notify: Arc<Notify>,
    ) -> io::Result<Self> {
        let connection =
            NonBlockFuseConnection::new_with_unprivileged(mount_options, mount_path).await?;
        Ok(Self {
            unmount_notify,
            mode: ConnectionMode::NonBlock(connection),
        })
    }

    pub async fn read_vectored(
        &self,
        header_buf: Vec<u8>,
        data_buf: Vec<u8>,
    ) -> Option<CompleteIoResult<(Vec<u8>, Vec<u8>), usize>> {
        let mut unmount_fut = pin!(self.unmount_notify.notified().fuse());
        let mut read_fut = pin!(self.inner_read_vectored(header_buf, data_buf).fuse());

        select! {
            _ = unmount_fut => None,
            res = read_fut => Some(res)
        }
    }

    async fn inner_read_vectored(
        &self,
        header_buf: Vec<u8>,
        data_buf: Vec<u8>,
    ) -> CompleteIoResult<(Vec<u8>, Vec<u8>), usize> {
        match &self.mode {
            ConnectionMode::Block(connection) => {
                connection.read_vectored(header_buf, data_buf).await
            }
            ConnectionMode::NonBlock(connection) => {
                connection.read_vectored(header_buf, data_buf).await
            }
        }
    }

    pub async fn write_vectored<
        T: Deref<Target = [u8]> + Unpin + Send,
        U: Deref<Target = [u8]> + Unpin + Send,
    >(
        &self,
        data: T,
        body_extend_data: Option<U>,
    ) -> CompleteIoResult<(T, Option<U>), usize> {
        match &self.mode {
            ConnectionMode::Block(connection) => {
                connection.write_vectored(data, body_extend_data).await
            }
            ConnectionMode::NonBlock(connection) => {
                connection.write_vectored(data, body_extend_data).await
            }
        }
    }
}

impl AsFd for FuseConnection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match &self.mode {
            ConnectionMode::Block(connection) => connection.file.as_fd(),
            ConnectionMode::NonBlock(connection) => connection.fd.as_fd(),
        }
    }
}

// --- Blocking connection (privileged, /dev/fuse) ---

#[derive(Debug)]
struct BlockFuseConnection {
    file: File,
    read: Mutex<()>,
    write: Mutex<()>,
}

impl BlockFuseConnection {
    fn new() -> io::Result<Self> {
        const DEV_FUSE: &str = "/dev/fuse";
        let file = OpenOptions::new().write(true).read(true).open(DEV_FUSE)?;
        Ok(Self {
            file,
            read: Mutex::new(()),
            write: Mutex::new(()),
        })
    }

    async fn read_vectored(
        &self,
        mut header_buf: Vec<u8>,
        mut data_buf: Vec<u8>,
    ) -> CompleteIoResult<(Vec<u8>, Vec<u8>), usize> {
        use std::io::Read;
        use std::mem::ManuallyDrop;
        use std::os::fd::{AsRawFd, FromRawFd};

        let _guard = self.read.lock().await;
        let fd = self.file.as_raw_fd();

        let ((header_buf, data_buf), res) = task::spawn_blocking(move || {
            let file = unsafe { File::from_raw_fd(fd) };
            let mut file = ManuallyDrop::new(file);
            let res = file.read_vectored(&mut [
                IoSliceMut::new(&mut header_buf),
                IoSliceMut::new(&mut data_buf),
            ]);
            ((header_buf, data_buf), res)
        })
        .await
        .unwrap();

        ((header_buf, data_buf), res)
    }

    async fn write_vectored<T: Deref<Target = [u8]> + Send, U: Deref<Target = [u8]> + Send>(
        &self,
        data: T,
        body_extend_data: Option<U>,
    ) -> CompleteIoResult<(T, Option<U>), usize> {
        let _guard = self.write.lock().await;

        let res = {
            let body_extend_data = body_extend_data.as_deref();
            match body_extend_data {
                None => (&self.file).write_vectored(&[IoSlice::new(data.deref())]),
                Some(body_extend_data) => (&self.file)
                    .write_vectored(&[IoSlice::new(data.deref()), IoSlice::new(body_extend_data)]),
            }
        };

        match res {
            Err(err) => ((data, body_extend_data), Err(err)),
            Ok(n) => ((data, body_extend_data), Ok(n)),
        }
    }
}

// --- Non-blocking connection (unprivileged, fusermount3) ---

#[derive(Debug)]
struct NonBlockFuseConnection {
    fd: AsyncFd<OwnedFd>,
}

impl NonBlockFuseConnection {
    async fn new_with_unprivileged(
        mount_options: MountOptions,
        mount_path: impl AsRef<Path>,
    ) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};

        let (sock0, sock1) = socket::socketpair(
            AddressFamily::Unix,
            SockType::SeqPacket,
            None,
            SockFlag::empty(),
        )
        .map_err(io::Error::from)?;

        let binary_path = find_fusermount3()?;
        let options = mount_options.build_with_unprivileged();

        debug!("mount options {:?}", options);

        let mount_path = mount_path.as_ref().as_os_str().to_os_string();

        let fd0 = sock0.as_raw_fd();
        let mut child = Command::new(binary_path)
            .env("_FUSE_COMMFD", fd0.to_string())
            .args(vec![OsString::from("-o"), options, mount_path])
            .spawn()?;

        if !child.wait().await?.success() {
            return Err(io::Error::other("fusermount run failed"));
        }

        let fd1 = sock1.as_raw_fd();
        let fd = task::spawn_blocking(move || {
            let mut buf = vec![];
            let mut cmsg_buf = nix::cmsg_space!([RawFd; 1]);
            let mut bufs = [IoSliceMut::new(&mut buf)];

            let msg =
                socket::recvmsg::<()>(fd1, &mut bufs[..], Some(&mut cmsg_buf), MsgFlags::empty())
                    .map_err(io::Error::from)?;

            let fd = if let Some(ControlMessageOwned::ScmRights(fds)) = msg.cmsgs()?.next() {
                if fds.is_empty() {
                    return Err(io::Error::other("no fuse fd"));
                }
                fds[0]
            } else {
                return Err(io::Error::other("get fuse fd failed"));
            };

            Ok(fd)
        })
        .await
        .unwrap()?;

        Self::set_fd_non_blocking(fd)?;
        Self::set_fd_cloexec(fd)?;

        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        Ok(Self {
            fd: AsyncFd::new(fd)?,
        })
    }

    fn set_fd_non_blocking(fd: RawFd) -> io::Result<()> {
        let fd = unsafe { BorrowedFd::borrow_raw(fd) };
        let flags = nix::fcntl::fcntl(fd, FcntlArg::F_GETFL).map_err(io::Error::from)?;
        let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        nix::fcntl::fcntl(fd, FcntlArg::F_SETFL(flags)).map_err(io::Error::from)?;
        Ok(())
    }

    fn set_fd_cloexec(fd: RawFd) -> io::Result<()> {
        let fd = unsafe { BorrowedFd::borrow_raw(fd) };
        let flags = nix::fcntl::fcntl(fd, FcntlArg::F_GETFD).map_err(io::Error::from)?;
        let flags = FdFlag::from_bits_truncate(flags) | FdFlag::FD_CLOEXEC;
        nix::fcntl::fcntl(fd, FcntlArg::F_SETFD(flags)).map_err(io::Error::from)?;
        Ok(())
    }

    fn read_vectored(&self, header_buf: Vec<u8>, data_buf: Vec<u8>) -> ReadVectoredFut<'_> {
        ReadVectoredFut {
            conn: self,
            header_buf: Some(header_buf),
            data_buf: Some(data_buf),
        }
    }

    fn write_vectored<
        T: Deref<Target = [u8]> + Unpin + Send,
        U: Deref<Target = [u8]> + Unpin + Send,
    >(
        &self,
        data: T,
        body_extend_data: Option<U>,
    ) -> WriteVectoredFut<'_, T, U> {
        WriteVectoredFut {
            conn: self,
            data: Some(data),
            body_extend_data: Some(body_extend_data),
        }
    }
}

struct ReadVectoredFut<'a> {
    conn: &'a NonBlockFuseConnection,
    header_buf: Option<Vec<u8>>,
    data_buf: Option<Vec<u8>>,
}

impl Future for ReadVectoredFut<'_> {
    type Output = CompleteIoResult<(Vec<u8>, Vec<u8>), usize>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let header_buf = this.header_buf.as_mut().expect("polled after completion");
        let data_buf = this.data_buf.as_mut().expect("polled after completion");

        let mut guard = match this.conn.fd.poll_read_ready(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => {
                let header_buf = this.header_buf.take().unwrap();
                let data_buf = this.data_buf.take().unwrap();
                return Poll::Ready(((header_buf, data_buf), Err(err)));
            }
            Poll::Ready(Ok(guard)) => guard,
        };

        match guard.try_io(|fd| {
            uio::readv(
                fd,
                &mut [IoSliceMut::new(header_buf), IoSliceMut::new(data_buf)],
            )
            .map_err(io::Error::from)
        }) {
            Ok(result) => {
                cx.waker().wake_by_ref();
                let header_buf = this.header_buf.take().unwrap();
                let data_buf = this.data_buf.take().unwrap();
                Poll::Ready(((header_buf, data_buf), result))
            }
            Err(_would_block) => Poll::Pending,
        }
    }
}

struct WriteVectoredFut<'a, T, U> {
    conn: &'a NonBlockFuseConnection,
    data: Option<T>,
    body_extend_data: Option<Option<U>>,
}

impl<T: Deref<Target = [u8]> + Unpin + Send, U: Deref<Target = [u8]> + Unpin + Send> Future
    for WriteVectoredFut<'_, T, U>
{
    type Output = CompleteIoResult<(T, Option<U>), usize>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let data = this.data.as_ref().expect("polled after completion");
        let body_extend_data = this
            .body_extend_data
            .as_ref()
            .expect("polled after completion");

        let mut guard = match this.conn.fd.poll_write_ready(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => {
                let data = this.data.take().unwrap();
                let body_extend_data = this.body_extend_data.take().unwrap();
                return Poll::Ready(((data, body_extend_data), Err(err)));
            }
            Poll::Ready(Ok(guard)) => guard,
        };

        match guard.try_io(|fd| {
            let body_extend_data = body_extend_data.as_deref();
            match body_extend_data {
                None => uio::writev(fd, &[IoSlice::new(data.deref())]),
                Some(body_extend_data) => uio::writev(
                    fd,
                    &[IoSlice::new(data.deref()), IoSlice::new(body_extend_data)],
                ),
            }
            .map_err(io::Error::from)
        }) {
            Ok(result) => {
                cx.waker().wake_by_ref();
                let data = this.data.take().unwrap();
                let body_extend_data = this.body_extend_data.take().unwrap();
                Poll::Ready(((data, body_extend_data), result))
            }
            Err(_would_block) => Poll::Pending,
        }
    }
}
