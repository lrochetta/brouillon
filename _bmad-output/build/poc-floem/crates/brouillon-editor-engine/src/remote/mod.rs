//! Remote editing, editor side: everything the explorer, the tabs and the status bar need to show
//! a server as if it were a folder, WITHOUT ever blocking the UI thread.
//!
//! * [`RemoteProvider`] extends the blocking [`crate::fs_provider::FsProvider`] with the few
//!   mutating calls; it is the only thing a real (helper-backed) SFTP client has to implement.
//! * [`RemoteHub`] owns a small lazily started worker pool; the UI thread submits jobs and drains
//!   results (same shape as the Link `Bridge` / `Pump`: a `wake` callback, then `drain`).
//! * [`ListingCache`] is what the tree reads: a missing folder is a "loading" row plus one request.
//! * [`FakeRemoteProvider`] is an in-memory server with configurable latency and failures. It
//!   backs the unit tests and the developer flag; it never opens a socket.
//!
//! No secret is ever stored here: a site profile only carries a [`sites::SecretRef`] placeholder.

pub mod cache;
pub mod credentials;
pub mod client;
pub mod error;
pub mod explorer;
pub mod fake;
pub mod helper;
pub mod hostkey;
pub mod hub;
pub mod local;
pub mod path;
pub mod provider;
pub mod saving;
pub mod sites;
pub mod state;
pub mod tree;

pub use cache::{Listing, ListingCache};
pub use error::{RemoteError, RemoteErrorKind};
pub use explorer::{Explorer, ExplorerNotice, OpenSite};
pub use fake::{DemoFactory, FakeConfig, FakeRemoteProvider};
pub use brouillon_safesave::{ConflictChoice, SaveOutcome};
pub use client::{CallError, HelperClient, NoPrompts, PromptHandler, Timeouts};
pub use hostkey::{forget_host_key, known_hosts_entry};
pub use credentials::{Credentials, KeepOffer, SavedEntry, SecretId};
pub use helper::{AskKind, AskView, PromptVault, HelperFactory, HelperFs, HelperProvider, PromptBroker, RealOrDemoFactory};
pub use hub::{JobId, OpKind, ProviderFactory, RemoteEvent, RemoteHub, RemoteOp, RemoteReply};
pub use local::{LocalProvider, LOCAL_SITE_ID};
pub use path::RemotePath;
pub use provider::{OpenStamp, RemoteEntry, RemoteProvider};
pub use saving::{save_as_suggestion, view_of, ConflictKind, DocKey, OutcomeView, RecoverItem, ResolvedView, SaveEvent, SaveService, Submit};
pub use sites::{AuthMethod, Protocol, SecretRef, SiteId, SiteProfile, SiteStore};
pub use state::{ChipTone, ConnState, RemoteChip, RemoteDocState, RemoteOrigin, RemoteSaveState};
pub use tree::{RemoteRow, RemoteRowKind, RemoteTree};

#[cfg(test)]
mod credentials_tests;
#[cfg(test)]
mod helper_tests;
