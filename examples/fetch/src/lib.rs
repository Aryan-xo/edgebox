//! Shows secrets and outbound HTTP.
//!
//! - `/fetch/env`          returns the `GREETING` environment variable
//! - `/fetch?url=<url>`    GETs `<url>` and returns its body (502 if not allowed)

use wasi::exports::http::incoming_handler::Guest;
use wasi::http::outgoing_handler;
use wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    ResponseOutparam, Scheme,
};
use wasi::io::streams::StreamError;

struct Fetch;

impl Guest for Fetch {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_default();

        if path == "/fetch/env" {
            let greeting =
                std::env::var("GREETING").unwrap_or_else(|_| "GREETING is not set".into());
            return respond(response_out, 200, format!("{greeting}\n").as_bytes());
        }

        let Some(url) = path.split_once("?url=").map(|(_, url)| url) else {
            return respond(
                response_out,
                400,
                b"usage: /fetch?url=<url> or /fetch/env\n",
            );
        };
        match get(url) {
            Ok(body) => respond(response_out, 200, &body),
            Err(err) => respond(
                response_out,
                502,
                format!("request failed: {err}\n").as_bytes(),
            ),
        }
    }
}

/// A minimal blocking HTTP GET over `wasi:http/outgoing-handler`.
fn get(url: &str) -> Result<Vec<u8>, String> {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else {
        return Err("url must start with http:// or https://".into());
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let path = if path.is_empty() { "/" } else { path };

    let request = OutgoingRequest::new(Fields::new());
    let invalid = |part: &str| format!("invalid {part} in {url:?}");
    request
        .set_method(&Method::Get)
        .map_err(|()| invalid("method"))?;
    request
        .set_scheme(Some(&scheme))
        .map_err(|()| invalid("scheme"))?;
    request
        .set_authority(Some(authority))
        .map_err(|()| invalid("host"))?;
    request
        .set_path_with_query(Some(path))
        .map_err(|()| invalid("path"))?;
    let request_body = request.body().expect("body is taken once");

    let pending = outgoing_handler::handle(request, None).map_err(|code| format!("{code:?}"))?;
    OutgoingBody::finish(request_body, None).map_err(|code| format!("{code:?}"))?;
    let response = loop {
        if let Some(result) = pending.get() {
            break result;
        }
        pending.subscribe().block();
    };
    let response = response
        .expect("response is taken once")
        .map_err(|code| format!("{code:?}"))?;

    let body = response.consume().expect("body is taken once");
    let stream = body.stream().expect("stream is taken once");
    let mut bytes = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) => bytes.extend(chunk),
            Err(StreamError::Closed) => return Ok(bytes),
            Err(err) => return Err(format!("{err:?}")),
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let response = OutgoingResponse::new(Fields::new());
    response.set_status_code(status).expect("valid status");
    let outgoing = response.body().expect("body is taken once");
    ResponseOutparam::set(response_out, Ok(response));
    let stream = outgoing.write().expect("stream is taken once");
    for chunk in body.chunks(4096) {
        stream.blocking_write_and_flush(chunk).expect("write body");
    }
    drop(stream);
    OutgoingBody::finish(outgoing, None).expect("finish body");
}

wasi::http::proxy::export!(Fetch);
