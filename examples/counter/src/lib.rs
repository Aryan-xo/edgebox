//! Counts visits in edgebox's KV store: `You are visitor #N`.

use wasi::exports::http::incoming_handler::Guest;
use wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

wit_bindgen::generate!({
    path: "../../wit",
    world: "imports",
});

use edgebox::kv::store;

struct Counter;

impl Guest for Counter {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        let visits = store::get("visits")
            .ok()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok()?.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;

        let text = match store::set("visits", visits.to_string().as_bytes()) {
            Ok(()) => format!("You are visitor #{visits}\n"),
            Err(err) => format!("could not save visit: {err}\n"),
        };
        respond(response_out, &text);
    }
}

fn respond(response_out: ResponseOutparam, text: &str) {
    let response = OutgoingResponse::new(Fields::new());
    let body = response.body().expect("body is taken once");
    ResponseOutparam::set(response_out, Ok(response));
    let stream = body.write().expect("stream is taken once");
    stream
        .blocking_write_and_flush(text.as_bytes())
        .expect("write body");
    drop(stream);
    OutgoingBody::finish(body, None).expect("finish body");
}

wasi::http::proxy::export!(Counter);
