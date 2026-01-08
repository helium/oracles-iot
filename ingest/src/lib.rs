extern crate tls_init;

pub mod server_chain;
pub mod server_iot;
// pub mod server_mobile;  // Commented out - will be pruned in post-split cleanup
pub mod settings;

pub use settings::{Mode, Settings};

#[cfg(test)]
tls_init::include_tls_tests!();
