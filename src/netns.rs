// Copyright (c) 2022 Alibaba Cloud
//
// SPDX-License-Identifier: Apache-2.0
//

use std::fs::File;
use std::mem::ManuallyDrop;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{setns, unshare, CloneFlags};
use nix::unistd::gettid;

use crate::{Error, Result};

/// Defines a NetNs environment behavior.
pub trait Env {
    /// The persist dir of the NetNs environment.
    fn persist_dir(&self) -> PathBuf;

    /// Returns `true` if the given path is in this Env.
    fn contains<P: AsRef<Path>>(&self, p: P) -> bool {
        p.as_ref().starts_with(self.persist_dir())
    }

    /// Initialize the environment.
    fn init(&self) -> Result<()> {
        // Create the directory for mounting network namespaces
        // This needs to be a shared mountpoint in case it is mounted in to
        // other namespaces (containers)
        let persist_dir = self.persist_dir();
        std::fs::create_dir_all(&persist_dir).map_err(Error::CreateNsDirError)?;

        // Remount the namespace directory shared. This will fail if it is not
        // already a mountpoint, so bind-mount it on to itself to "upgrade" it
        // to a mountpoint.
        let mut made_netns_persist_dir_mount: bool = false;
        while let Err(e) = mount(
            Some(""),
            &persist_dir,
            Some("none"),
            MsFlags::MS_SHARED | MsFlags::MS_REC,
            Some(""),
        ) {
            // Fail unless we need to make the mount point
            if e != nix::errno::Errno::EINVAL || made_netns_persist_dir_mount {
                return Err(Error::MountError(
                    format!("--make-rshared {}", persist_dir.display()),
                    e,
                ));
            }
            // Recursively remount /var/persist/netns on itself. The recursive flag is
            // so that any existing netns bindmounts are carried over.
            mount(
                Some(&persist_dir),
                &persist_dir,
                Some("none"),
                MsFlags::MS_BIND | MsFlags::MS_REC,
                Some(""),
            )
            .map_err(|e| {
                Error::MountError(
                    format!(
                        "-rbind {} to {}",
                        persist_dir.display(),
                        persist_dir.display()
                    ),
                    e,
                )
            })?;
            made_netns_persist_dir_mount = true;
        }

        Ok(())
    }
}

/// A default network namespace environment. Its persistence directory is `/var/run/netns`,
/// which is for consistency with the `ip-netns` tool.
/// See [ip-netns](https://man7.org/linux/man-pages/man8/ip-netns.8.html) for details.
#[derive(Copy, Clone, Default, Debug)]
pub struct DefaultEnv;

impl Env for DefaultEnv {
    fn persist_dir(&self) -> PathBuf {
        PathBuf::from("/var/run/netns")
    }
}

/// A network namespace type.
///
/// It could be used to enter network namespace.
#[derive(Debug)]
pub struct NetNs<E: Env = DefaultEnv> {
    file: ManuallyDrop<File>,
    path: PathBuf,
    env: Option<E>,
    file_dropped: bool,
}

impl<E: Env> std::fmt::Display for NetNs<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Ok(meta) = self.file.metadata() {
            write!(
                f,
                "NetNS {{ fd: {}, dev: {}, ino: {}, path: {} }}",
                self.file.as_raw_fd(),
                meta.dev(),
                meta.ino(),
                self.path.display()
            )
        } else {
            write!(
                f,
                "NetNS {{ fd: {}, path: {} }}",
                self.file.as_raw_fd(),
                self.path.display()
            )
        }
    }
}

impl<E1: Env, E2: Env> PartialEq<NetNs<E1>> for NetNs<E2> {
    fn eq(&self, other: &NetNs<E1>) -> bool {
        if self.file.as_raw_fd() == other.file.as_raw_fd() {
            return true;
        }
        let cmp_meta = |f1: &File, f2: &File| -> Option<bool> {
            let m1 = match f1.metadata() {
                Ok(m) => m,
                Err(_) => return None,
            };
            let m2 = match f2.metadata() {
                Ok(m) => m,
                Err(_) => return None,
            };
            Some(m1.dev() == m2.dev() && m1.ino() == m2.ino())
        };
        cmp_meta(&self.file, &other.file).unwrap_or_else(|| self.path == other.path)
    }
}

impl<E: Env> Drop for NetNs<E> {
    fn drop(&mut self) {
        if !self.file_dropped {
            unsafe { ManuallyDrop::drop(&mut self.file) };
        }
    }
}

impl<E: Env> NetNs<E> {
    /// Creates a new `NetNs` with the specified name and Env.
    /// The persist dir of network namespace will be created if it doesn't already exist.
    pub fn new_with_env<S: AsRef<str>>(ns_name: S, env: E) -> Result<Self> {
        env.init()?;

        // create an empty file at the mount point
        let ns_path = env.persist_dir().join(ns_name.as_ref());
        let _ = File::create(&ns_path).map_err(Error::CreateNsError)?;
        Self::persistent(&ns_path, true).map_err(|e| {
            // Ensure the mount point is cleaned up on errors; if the namespace
            // was successfully mounted this will have no effect because the file
            // is in-use
            std::fs::remove_file(&ns_path).ok();
            e
        })?;
        Self::get_from_env(ns_name, env)
    }

    fn persistent<P: AsRef<Path>>(ns_path: &P, new_thread: bool) -> Result<()> {
        if new_thread {
            let ns_path_clone = ns_path.as_ref().to_path_buf();
            let new_thread: JoinHandle<Result<()>> =
                thread::spawn(move || Self::persistent(&ns_path_clone, false));
            match new_thread.join() {
                Ok(t) => {
                    if let Err(e) = t {
                        return Err(e);
                    }
                }
                Err(e) => {
                    return Err(Error::JoinThreadError(format!("{:?}", e)));
                }
            };
        } else {
            // Create a new netns on the current thread.
            unshare(CloneFlags::CLONE_NEWNET).map_err(Error::UnshareError)?;
            // bind mount the netns from the current thread (from /proc) onto the
            // mount point. This causes the namespace to persist, even when there
            // are no threads in the ns.
            let src = get_current_thread_netns_path();
            mount(
                Some(src.as_path()),
                ns_path.as_ref(),
                Some("none"),
                MsFlags::MS_BIND,
                Some(""),
            )
            .map_err(|e| {
                Error::MountError(
                    format!("-rbind {} to {}", src.display(), ns_path.as_ref().display()),
                    e,
                )
            })?;
        }
        Ok(())
    }

    /// Gets the path of this network namespace.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Gets the Env of this network namespace.
    pub fn env(&self) -> Option<&E> {
        self.env.as_ref()
    }

    /// Gets the Env of this network namespace.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Makes the current thread enter this network namespace.
    ///
    /// Requires elevated privileges.
    pub fn enter(&self) -> Result<()> {
        setns(self.file.as_raw_fd(), CloneFlags::CLONE_NEWNET).map_err(Error::SetnsError)
    }

    /// Returns the NetNs with the specified name and Env.
    pub fn get_from_env<S: AsRef<str>>(ns_name: S, env: E) -> Result<Self> {
        let ns_path = env.persist_dir().join(ns_name.as_ref());
        let file = File::open(&ns_path).map_err(|e| Error::OpenNsError(ns_path.clone(), e))?;

        Ok(Self {
            file: ManuallyDrop::new(file),
            path: ns_path,
            env: Some(env),
            file_dropped: false,
        })
    }

    /// Removes this network namespace manually.
    ///
    /// Once called, this instance will not be available.
    pub fn umount(&mut self) -> Result<()> {
        // need close first
        nix::unistd::close(self.file.as_raw_fd()).map_err(Error::CloseNsError)?;
        self.file_dropped = true;
        self.umount_ns()
    }

    fn umount_ns(&mut self) -> Result<()> {
        // Only unmount if it's been bind-mounted (don't touch namespaces in /proc...)
        if let Some(env) = &self.env {
            if env.contains(&self.path) {
                umount2(&self.path, MntFlags::MNT_DETACH)
                    .map_err(|e| Error::UnmountError(self.path.clone(), e))?;
                std::fs::remove_file(&self.path)
                    .map_err(|e| Error::RemoveNsError(self.path.clone(), e))
                    .ok();
            }
        }
        Ok(())
    }

    /// Run a closure in NetNs, which is specified by name and Env.
    ///
    /// Requires elevated privileges.
    pub fn run<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Self) -> T,
    {
        // get current network namespace
        let src_ns = get_from_current_thread()?;

        // do nothing if ns_path is same as current_ns
        if &src_ns == self {
            return Ok(f(self));
        }
        // enter new namespace
        self.enter()?;

        let result = f(self);
        // back to old namespace
        src_ns.enter()?;

        Ok(result)
    }
}

impl NetNs {
    /// Creates a new persistent (bind-mounted) network namespace and returns an object representing
    /// that namespace, without switching to it.
    ///
    /// The persist dir of network namespace will be created if it doesn't already exist. This function
    /// will use [`DefaultEnv`] to create persist dir.
    ///
    /// Requires elevated privileges.
    ///
    /// [`DefaultEnv`]: DefaultEnv
    ///
    pub fn new<S: AsRef<str>>(ns_name: S) -> Result<Self> {
        Self::new_with_env(ns_name, DefaultEnv)
    }

    /// Returns the NetNs with the specified name and `DefaultEnv`.
    pub fn get<S: AsRef<str>>(ns_name: S) -> Result<Self> {
        Self::get_from_env(ns_name, DefaultEnv)
    }

    /// Run a closure in NetNs, which is specified by name and `DefaultEnv`.
    ///
    /// Requires elevated privileges.
    pub fn run_in<S, F, T>(ns_name: S, f: F) -> Result<T>
    where
        S: AsRef<str>,
        F: FnOnce(&Self) -> T,
    {
        // get network namespace
        let run_ns = Self::get_from_env(ns_name, DefaultEnv)?;
        run_ns.run(f)
    }
}

/// Returns the NetNs with the spectified path.
pub fn get_from_path<P: AsRef<Path>>(ns_path: P) -> Result<NetNs> {
    let ns_path = ns_path.as_ref().to_path_buf();
    let file = File::open(&ns_path).map_err(|e| Error::OpenNsError(ns_path.clone(), e))?;
    Ok(NetNs {
        file: ManuallyDrop::new(file),
        path: ns_path,
        env: None,
        file_dropped: false,
    })
}

/// Returns the NetNs of current thread.
pub fn get_from_current_thread() -> Result<NetNs> {
    let ns_path = get_current_thread_netns_path();
    let file = File::open(&ns_path).map_err(|e| Error::OpenNsError(ns_path.clone(), e))?;
    Ok(NetNs {
        file: ManuallyDrop::new(file),
        path: ns_path,
        env: None,
        file_dropped: false,
    })
}

#[inline]
fn get_current_thread_netns_path() -> PathBuf {
    PathBuf::from(format!("/proc/self/task/{}/ns/net", gettid()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;

    #[test]
    fn test_netns_display() {
        let ns = get_from_current_thread().unwrap();
        let print = format!("{}", ns);
        assert!(print.contains("dev"));
        assert!(print.contains("ino"));

        let ns: NetNs<DefaultEnv> = NetNs {
            file: ManuallyDrop::new(unsafe { File::from_raw_fd(i32::MAX) }),
            path: PathBuf::from(""),
            env: None,
            file_dropped: true,
        };
        let print = format!("{}", ns);
        assert!(!print.contains("dev"));
        assert!(!print.contains("ino"));
    }

    #[test]
    fn test_netns_eq() {
        let ns1 = get_from_current_thread().unwrap();
        let ns2 = get_from_path("/proc/self/ns/net").unwrap();
        assert_eq!(ns1, ns2);

        let ns1: NetNs<DefaultEnv> = NetNs {
            file: ManuallyDrop::new(unsafe { File::from_raw_fd(i32::MAX) }),
            path: PathBuf::from("aaaaaa"),
            env: None,
            file_dropped: true,
        };
        let ns2: NetNs<DefaultEnv> = NetNs {
            file: ManuallyDrop::new(unsafe { File::from_raw_fd(i32::MAX) }),
            path: PathBuf::from("bbbbbb"),
            env: None,
            file_dropped: true,
        };
        assert_eq!(ns1, ns2);

        let ns2: NetNs<DefaultEnv> = NetNs {
            file: ManuallyDrop::new(unsafe { File::from_raw_fd(i32::MAX - 1) }),
            path: PathBuf::from("aaaaaa"),
            env: None,
            file_dropped: true,
        };
        assert_eq!(ns1, ns2);
    }

    #[test]
    fn test_netns_init() {
        let mut ns = NetNs::new("test_netns_init").unwrap();
        assert!(ns.path().exists());
        ns.umount().unwrap();
        assert!(!Path::new(&DefaultEnv.persist_dir())
            .join("test_netns_init")
            .exists());
    }

    struct TestNetNs {
        netns: NetNs,
        ns_name: String,
    }

    impl TestNetNs {
        fn new(name: &str) -> Self {
            let netns = NetNs::new(name).unwrap();
            assert!(netns.path().exists());
            Self {
                netns,
                ns_name: String::from(name),
            }
        }
    }

    impl Drop for TestNetNs {
        fn drop(&mut self) {
            let ns_name = self.ns_name.clone();
            self.netns.umount().unwrap();
            assert!(!Path::new(&DefaultEnv.persist_dir()).join(ns_name).exists());
        }
    }

    #[test]
    fn test_netns_enter() {
        let new = TestNetNs::new("test_netns_enter");

        let src = get_from_current_thread().unwrap();
        assert_ne!(src, new.netns);

        new.netns.enter().unwrap();

        let cur = get_from_current_thread().unwrap();

        assert_eq!(new.netns, cur);
        assert_ne!(src, cur);
        assert_ne!(src, new.netns);
    }

    struct TestEnv;
    impl Env for TestEnv {
        fn persist_dir(&self) -> PathBuf {
            PathBuf::from("/tmp/test_netns")
        }
    }

    #[test]
    fn test_netns_with_env() {
        let ns_res = NetNs::get_from_env("test_netns_run", TestEnv);
        assert!(matches!(ns_res, Err(Error::OpenNsError(_, _))));

        let mut ns = NetNs::new_with_env("test_netns_run", TestEnv).unwrap();
        assert!(ns.path().exists());

        ns.umount().unwrap();
        assert!(!Path::new(&TestEnv.persist_dir())
            .join("test_netns_set")
            .exists());
    }

    #[test]
    fn test_netns_run() {
        let new = TestNetNs::new("test_netns_run");

        let src_ns = get_from_current_thread().unwrap();

        let ret = new
            .netns
            .run(|cur_ns| -> Result<()> {
                let cur_thread = get_from_current_thread().unwrap();
                assert_eq!(cur_ns, &cur_thread);
                // captured variables
                assert_eq!(cur_ns, &new.netns);
                assert_ne!(cur_ns, &src_ns);

                Ok(())
            })
            .unwrap();
        assert!(matches!(ret, Ok(_)));
    }
}
