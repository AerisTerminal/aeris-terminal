//! cTrader Open API wire framing and endpoint selection.

#[allow(dead_code, clippy::all, clippy::pedantic)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/ctrader.protobuf.rs"));
}

pub use generated::ProtoMessage;

pub mod accounts;
pub mod codec;
pub mod host;
pub mod hosted;
pub mod market;
pub mod session;
pub mod transport;

#[cfg(test)]
mod session_tests;
