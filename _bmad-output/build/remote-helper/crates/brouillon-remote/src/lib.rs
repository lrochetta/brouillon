//! Brouillon remote helper: an SFTP v3 client driven through the OS `ssh`, exposed to the editor
//! through the framed protocol of `brouillon-remote-proto`.

pub mod askpass;
pub mod backend;
pub mod ftp;
pub mod helper;
pub mod ops;
pub mod sftp;
pub mod testkit;
pub mod transport;

