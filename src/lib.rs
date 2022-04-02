// Copyright 2022 Alibaba Cloud. All Rights Reserved.
//
// SPDX-License-Identifier: Apache-2.0
//

//! This crate provides an ultra-simple interface for handling network namespaces
//! in Rust. Changing namespaces requires elevated privileges, so in most cases this
//! code needs to be run as root.
//!
//! We can simply create a NetNs using [`NetNs::new`]. Once created, the netns
//! instance can be used.
//!
//! # Examples
//!
//!```no_run
//!use netns_rs::NetNs;
//!
//!// create a new netns in `/var/run/netns` by default.
//!let mut ns = NetNs::new("my_netns").unwrap();
//!
//!ns.run(|_| {
//!    // do something in the new netns. eg. ip link add.
//!}).unwrap();
//!
//!// removes netns.
//!ns.remove().unwrap();
//!```
//! To get a Netns that already exists, you can use the [`NetNs::get`] series of functions.
//!```no_run
//!use netns_rs::NetNs;
//!
//!let ns = NetNs::get("my_netns").unwrap();
//!```
//! Or use [`get_from_current_thread`] to get the netns of the current thread.
//!```no_run
//!use netns_rs::get_from_current_thread;
//!
//!let ns = get_from_current_thread().unwrap();
//!```
//! [`NetNs::new`]: NetNs::new
//! [`NetNs::get`]: NetNs::get
//! [`get_from_current_thread`]: get_from_current_thread

mod netns;
pub use self::netns::*;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Can not create netns directory, {0}")]
    CreateNsDirError(std::io::Error),

    #[error("Can not create netns,{0}")]
    CreateNsError(std::io::Error),

    #[error("Can not open netns {0},{1}")]
    OpenNsError(std::path::PathBuf, std::io::Error),

    #[error("Failed to close netns,{0}")]
    CloseNsError(nix::Error),

    #[error("Can not remove netns {0}, {1}")]
    RemoveNsError(std::path::PathBuf, std::io::Error),

    #[error("Failed to mount {0}, {1}")]
    MountError(String, nix::Error),

    #[error("Failed to unmount {0}, {1}")]
    UnmountError(std::path::PathBuf, nix::Error),

    #[error("Failed to unshare, {0}")]
    UnshareError(nix::Error),

    #[error("Failed to join thread, {0}")]
    JoinThreadError(String),

    #[error("Can not setns, {0}")]
    SetnsError(nix::Error),
}
