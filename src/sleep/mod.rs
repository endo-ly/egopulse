//! Sleep batch execution and scheduling.

pub(crate) mod scheduler;

mod batch;

pub use batch::{SleepBatchError, run_sleep_batch};
