//! Live app transport, event storage and agent control API.

mod app_channel;
pub mod control;
pub mod events;
pub mod inputs;
pub mod output;
mod process;
pub mod protocol;
pub mod session;
pub mod timing;
pub mod windows;

pub use app_channel::DevServer;

#[cfg(test)]
mod tests;
