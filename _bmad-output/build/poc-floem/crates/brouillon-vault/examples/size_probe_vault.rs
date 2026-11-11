//! Worst-case reachable vault code (create, put, read, replace, delete, lock) for release-size
//! measurement: `cargo build --release -p brouillon-vault --examples`, compare with `size_probe_base`.
use brouillon_vault::*;
use std::path::PathBuf;

fn main() {
    let p = PathBuf::from(std::env::args().nth(1).unwrap_or_default());
    let v = FileVault::new(&p, VaultPolicy::default());
    v.unlock(Unlock::Os).unwrap();
    let id = v
        .put(NewSecret { kind: SecretKind::Password, label: "l".into(), scope: "s".into() }, Secret::from_bytes(b"x"))
        .unwrap();
    v.replace(id, Secret::from_bytes(b"y")).unwrap();
    println!("{:?}", v.list().unwrap().len());
    println!("{}", v.with_secret(id, Purpose::RemoteLogin, |s| s.len()).unwrap());
    v.delete(id).unwrap();
    v.lock();
}
