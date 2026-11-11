use std::future::Future;

pub use futures_executor::block_on;

pub mod runtime {
    use super::*;

    pub struct Runtime;

    impl Runtime {
        pub fn enter(&self) -> RuntimeGuard {
            RuntimeGuard
        }

        pub fn spawn<F>(&self, future: F)
        where
            F: Future + Send + 'static,
            F::Output: Send + 'static,
        {
            std::thread::spawn(move || {
                futures_executor::block_on(future);
            });
        }
    }

    pub struct RuntimeGuard;

    pub struct Builder;

    impl Builder {
        pub fn new_multi_thread() -> Self {
            Self
        }
        pub fn enable_all(&mut self) -> &mut Self {
            self
        }
        pub fn build(&mut self) -> Result<Runtime, std::io::Error> {
            Ok(Runtime)
        }
    }
}

pub mod process {
    use std::process::Stdio;

    pub struct Command {
        inner: std::process::Command,
    }

    impl Command {
        pub fn new(program: &str) -> Self {
            Self {
                inner: std::process::Command::new(program),
            }
        }
        pub fn args<I, S>(&mut self, args: I) -> &mut Self
        where
            I: IntoIterator<Item = S>,
            S: AsRef<std::ffi::OsStr>,
        {
            self.inner.args(args);
            self
        }
        pub fn stdin(&mut self, cfg: Stdio) -> &mut Self {
            self.inner.stdin(cfg);
            self
        }
        pub fn stdout(&mut self, cfg: Stdio) -> &mut Self {
            self.inner.stdout(cfg);
            self
        }
        pub fn stderr(&mut self, cfg: Stdio) -> &mut Self {
            self.inner.stderr(cfg);
            self
        }
        pub fn spawn(&mut self) -> Result<Child, std::io::Error> {
            let mut child = self.inner.spawn()?;
            Ok(Child {
                stdin: child.stdin.take().map(|s| ChildStdin { inner: s }),
                stdout: child.stdout.take().map(|s| ChildStdout { inner: s }),
                stderr: child.stderr.take().map(|s| ChildStderr { inner: s }),
                inner: std::sync::Arc::new(std::sync::Mutex::new(Some(child))),
            })
        }
    }

    pub struct Child {
        pub stdin: Option<ChildStdin>,
        pub stdout: Option<ChildStdout>,
        pub stderr: Option<ChildStderr>,
        inner: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    }

    impl Child {
        pub fn start_kill(&mut self) -> Result<(), std::io::Error> {
            if let Some(mut child) = self.inner.lock().unwrap().take() {
                child.kill()?;
            }
            Ok(())
        }
    }

    pub struct ChildStdin {
        pub(crate) inner: std::process::ChildStdin,
    }

    impl ChildStdin {
        pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), std::io::Error> {
            use std::io::Write;
            let mut stdin = &self.inner;
            let mut buf_slice = buf;
            while !buf_slice.is_empty() {
                let n = (&mut stdin).write(buf_slice)?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ));
                }
                buf_slice = &buf_slice[n..];
            }
            Ok(())
        }

        pub async fn flush(&mut self) -> Result<(), std::io::Error> {
            use std::io::Write;
            let mut stdin = &self.inner;
            stdin.flush()
        }
    }

    pub struct ChildStdout {
        pub(crate) inner: std::process::ChildStdout,
    }

    impl std::io::Read for ChildStdout {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    pub struct ChildStderr {
        pub(crate) inner: std::process::ChildStderr,
    }

    impl std::io::Read for ChildStderr {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }
}

pub mod sync {
    pub mod mpsc {
        use futures_util::StreamExt;
        use std::fmt;

        pub fn unbounded_channel<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
            let (tx, rx) = futures_channel::mpsc::unbounded::<T>();
            (UnboundedSender { tx }, UnboundedReceiver { rx })
        }

        #[derive(Clone)]
        pub struct UnboundedSender<T> {
            tx: futures_channel::mpsc::UnboundedSender<T>,
        }

        impl<T> UnboundedSender<T> {
            pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
                self.tx.unbounded_send(msg).map_err(|e| SendError(e.into_inner()))
            }
        }

        pub struct UnboundedReceiver<T> {
            rx: futures_channel::mpsc::UnboundedReceiver<T>,
        }

        impl<T> UnboundedReceiver<T> {
            pub async fn recv(&mut self) -> Option<T> {
                self.rx.next().await
            }
        }

        #[derive(Debug)]
        pub struct SendError<T>(pub T);

        impl<T> fmt::Display for SendError<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "SendError")
            }
        }

        impl<T: fmt::Debug> std::error::Error for SendError<T> {}
    }
}

pub mod io {
    use std::io::BufRead;

    pub trait AsyncBufReadExt {}
    impl<T> AsyncBufReadExt for T {}

    pub trait AsyncWriteExt {}
    impl<T> AsyncWriteExt for T {}

    pub struct BufReader<R> {
        inner: std::io::BufReader<R>,
    }

    impl<R: std::io::Read> BufReader<R> {
        pub fn new(inner: R) -> Self {
            Self {
                inner: std::io::BufReader::new(inner),
            }
        }

        pub fn lines(self) -> Lines<R> {
            Lines {
                inner: self.inner.lines(),
            }
        }
    }

    pub struct Lines<R> {
        inner: std::io::Lines<std::io::BufReader<R>>,
    }

    impl<R: std::io::Read> Lines<R> {
        pub async fn next_line(&mut self) -> Result<Option<String>, std::io::Error> {
            match self.inner.next() {
                Some(res) => res.map(Some),
                None => Ok(None),
            }
        }
    }
}

pub mod time {
    pub async fn sleep(duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
    pub use std::time::Duration;
}
