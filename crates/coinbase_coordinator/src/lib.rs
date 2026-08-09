//! Persistent Coinbase market-data coordinator shared by the desktop shell and
//! the resident data-engine process.

pub mod live_market_worker;
pub mod market_worker;
mod rithmic_history;
pub mod rithmic_live_chart;
pub mod rithmic_market_worker;
pub mod rithmic_series;
pub mod rithmic_shell;
mod rithmic_transition_capture;
