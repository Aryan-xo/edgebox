//! An HTTP function that greets the caller with the path they requested.

use wasi::exports::http::incoming_handler::Guest;
use wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct HttpHello;

impl Guest for HttpHello {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_default();
        let text = format!("Hello from WASM! You requested {path}\n");
        println!("handled {path}"); // shows up in `edgebox logs hello`

        // A known length lets the server reuse the connection, even for HTTP/1.0 clients.
        let headers = Fields::from_list(&[
            ("content-type".into(), b"text/plain".to_vec()),
            ("content-length".into(), text.len().to_string().into_bytes()),
        ])
        .expect("valid headers");
        let response = OutgoingResponse::new(headers);
        let body = response.body().expect("body is taken once");
        ResponseOutparam::set(response_out, Ok(response));

        let stream = body.write().expect("stream is taken once");
        stream
            .blocking_write_and_flush(text.as_bytes())
            .expect("write body");
        drop(stream); // the stream must be dropped before the body is finished
        OutgoingBody::finish(body, None).expect("finish body");
    }
}

wasi::http::proxy::export!(HttpHello);
