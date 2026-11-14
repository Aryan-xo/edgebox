//! A badly behaved HTTP function, for testing sandbox limits.
//!
//! - `/chaos/loop`  spins forever (CPU)
//! - `/chaos/sleep` blocks for a minute (wall clock)
//! - `/chaos/alloc` allocates until memory runs out
//! - anything else  responds `ok`

use std::hint::black_box;
use std::time::Duration;

use wasi::exports::http::incoming_handler::Guest;
use wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Chaos;

impl Guest for Chaos {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_default();
        match path.as_str() {
            "/chaos/loop" => {
                let mut n: u64 = 0;
                loop {
                    n = black_box(n.wrapping_add(1));
                }
            }
            "/chaos/sleep" => std::thread::sleep(Duration::from_secs(60)),
            "/chaos/alloc" => {
                let mut chunks = Vec::new();
                loop {
                    chunks.push(black_box(vec![1u8; 1024 * 1024]));
                }
            }
            _ => {}
        }

        let response = OutgoingResponse::new(Fields::new());
        let body = response.body().expect("body is taken once");
        ResponseOutparam::set(response_out, Ok(response));
        let stream = body.write().expect("stream is taken once");
        stream
            .blocking_write_and_flush(b"ok\n")
            .expect("write body");
        drop(stream);
        OutgoingBody::finish(body, None).expect("finish body");
    }
}

wasi::http::proxy::export!(Chaos);
