//! Prometheus metrics in the text exposition format, without a global registry.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::Duration;

/// Upper bounds of the latency histogram buckets, in seconds.
const BUCKETS: [f64; 11] = [
    0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0,
];

#[derive(Default)]
struct FunctionMetrics {
    by_status: BTreeMap<u16, u64>,
    /// Cumulative: `buckets[i]` counts requests that took at most `BUCKETS[i]`.
    buckets: [u64; BUCKETS.len()],
    sum_seconds: f64,
    count: u64,
}

/// Request counts and latencies per function.
///
/// Only deployed functions are recorded, so label cardinality stays bounded.
#[derive(Default)]
pub struct Metrics {
    functions: Mutex<BTreeMap<String, FunctionMetrics>>,
}

impl Metrics {
    pub fn record(&self, function: &str, status: u16, duration: Duration) {
        let seconds = duration.as_secs_f64();
        let mut functions = self.functions.lock().unwrap();
        let m = functions.entry(function.to_string()).or_default();
        *m.by_status.entry(status).or_default() += 1;
        for (bucket, le) in m.buckets.iter_mut().zip(BUCKETS) {
            if seconds <= le {
                *bucket += 1;
            }
        }
        m.sum_seconds += seconds;
        m.count += 1;
    }

    pub fn remove(&self, function: &str) {
        self.functions.lock().unwrap().remove(function);
    }

    /// Renders all metrics. Function names are already safe label values.
    pub fn render(&self, deployed_functions: usize) -> String {
        let functions = self.functions.lock().unwrap();
        let mut out = String::new();

        out.push_str(
            "# HELP edgebox_requests_total Requests handled, by function and HTTP status.\n",
        );
        out.push_str("# TYPE edgebox_requests_total counter\n");
        for (name, m) in functions.iter() {
            for (status, count) in &m.by_status {
                let _ = writeln!(
                    out,
                    "edgebox_requests_total{{function=\"{name}\",status=\"{status}\"}} {count}"
                );
            }
        }

        out.push_str("# HELP edgebox_request_duration_seconds Time until the function returned response headers.\n");
        out.push_str("# TYPE edgebox_request_duration_seconds histogram\n");
        for (name, m) in functions.iter() {
            for (count, le) in m.buckets.iter().zip(BUCKETS) {
                let _ = writeln!(
                    out,
                    "edgebox_request_duration_seconds_bucket{{function=\"{name}\",le=\"{le}\"}} {count}"
                );
            }
            let _ = writeln!(
                out,
                "edgebox_request_duration_seconds_bucket{{function=\"{name}\",le=\"+Inf\"}} {}",
                m.count
            );
            let _ = writeln!(
                out,
                "edgebox_request_duration_seconds_sum{{function=\"{name}\"}} {}",
                m.sum_seconds
            );
            let _ = writeln!(
                out,
                "edgebox_request_duration_seconds_count{{function=\"{name}\"}} {}",
                m.count
            );
        }

        out.push_str("# HELP edgebox_functions Deployed functions.\n");
        out.push_str("# TYPE edgebox_functions gauge\n");
        let _ = writeln!(out, "edgebox_functions {deployed_functions}");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_and_cumulative_histogram() {
        let metrics = Metrics::default();
        metrics.record("hello", 200, Duration::from_micros(50));
        metrics.record("hello", 200, Duration::from_millis(3));
        metrics.record("hello", 504, Duration::from_secs(20));

        let text = metrics.render(1);
        assert!(text.contains(r#"edgebox_requests_total{function="hello",status="200"} 2"#));
        assert!(text.contains(r#"edgebox_requests_total{function="hello",status="504"} 1"#));
        assert!(text.contains(
            r#"edgebox_request_duration_seconds_bucket{function="hello",le="0.0001"} 1"#
        ));
        assert!(
            text.contains(
                r#"edgebox_request_duration_seconds_bucket{function="hello",le="0.005"} 2"#
            )
        );
        assert!(
            text.contains(r#"edgebox_request_duration_seconds_bucket{function="hello",le="10"} 2"#)
        );
        assert!(
            text.contains(
                r#"edgebox_request_duration_seconds_bucket{function="hello",le="+Inf"} 3"#
            )
        );
        assert!(text.contains(r#"edgebox_request_duration_seconds_count{function="hello"} 3"#));
        assert!(text.contains("edgebox_functions 1"));

        metrics.remove("hello");
        assert!(!metrics.render(0).contains("hello"));
    }
}
