#![allow(unused_variables)]

pub use stacks::monitoring::{increment_errors_emitted_counter, increment_warning_emitted_counter};

mod prometheus;

pub fn start_serving_monitoring_metrics(bind_address: String) {
    info!("Start serving prometheus metrics {}", bind_address);
    prometheus::start_serving_prometheus_metrics(bind_address);
}
