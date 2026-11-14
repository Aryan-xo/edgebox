//! Serving HTTP requests with `wasi:http/proxy` components.

use std::error::Error as StdError;
use std::future::Future;
use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::body::{Body, Bytes};
use tokio::sync::oneshot;
use wasmtime::{Result, bail};
use wasmtime_wasi::WasiCtx;
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::bindings::http::types::Scheme;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::{
    Error as HttpError, RequestOptions, WasiBody, WasiHttpHooks, WasiHttpView,
};

use crate::State;
use crate::kv::Kv;
use crate::limits::{self, Limits, Timeout};
use crate::logs::{LogPipe, LogSink, LogStream};

/// Body of a response produced by an [`HttpFunction`].
pub type ResponseBody = HyperOutgoingBody;

/// What a function may access beyond its own memory. Everything is off by default.
#[derive(Clone, Default)]
pub struct Settings {
    /// Environment variables (e.g. secrets) the function can read.
    pub env: Vec<(String, String)>,
    /// Hosts the function may send HTTP requests to: exact names, or
    /// `*.example.com` for any subdomain.
    pub allowed_hosts: Vec<String>,
    /// The function's KV store.
    pub kv: Option<Kv>,
    /// Where the function's stdout/stderr go. `None` inherits the host's.
    pub logs: Option<LogSink>,
}

impl Settings {
    fn allows_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.allowed_hosts.iter().any(|allowed| {
            let allowed = allowed.to_ascii_lowercase();
            match allowed.strip_prefix("*.") {
                Some(domain) => host.ends_with(&format!(".{domain}")),
                None => host == allowed,
            }
        })
    }
}

/// A compiled HTTP function, ready to handle requests.
///
/// Cheap to clone. Every request runs in a fresh, isolated instance.
#[derive(Clone)]
pub struct HttpFunction {
    pre: ProxyPre<State>,
    limits: Limits,
    settings: Arc<Settings>,
}

impl HttpFunction {
    pub(crate) fn new(pre: ProxyPre<State>, limits: Limits) -> Self {
        Self {
            pre,
            limits,
            settings: Arc::default(),
        }
    }

    /// The same compiled function with different settings. No recompilation.
    pub fn with_settings(&self, settings: Settings) -> Self {
        Self {
            settings: Arc::new(settings),
            ..self.clone()
        }
    }

    /// Runs the function on `req` and returns its response.
    ///
    /// Fails with [`Timeout`] if the function takes longer than its time limit.
    /// Must be called from within a Tokio runtime.
    pub async fn handle<B>(&self, req: hyper::Request<B>) -> Result<hyper::Response<ResponseBody>>
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
    {
        let req = req
            .map(|body| body.map_err(|err| HttpError::InternalError(Some(err.into().to_string()))));

        let mut wasi = WasiCtx::builder();
        wasi.envs(&self.settings.env);
        match &self.settings.logs {
            Some(sink) => {
                let pipe = |stream| LogPipe {
                    sink: sink.clone(),
                    stream,
                };
                wasi.stdout(pipe(LogStream::Stdout))
                    .stderr(pipe(LogStream::Stderr));
            }
            None => {
                wasi.inherit_stdout().inherit_stderr();
            }
        }
        let wasi = wasi.build();
        let mut store =
            State::new_store(self.pre.engine(), wasi, &self.limits, self.settings.clone());

        let (sender, receiver) = oneshot::channel();
        let req = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, req)?;
        let out = store.data_mut().http().new_response_outparam(sender)?;

        // Run the guest in its own task: it can keep streaming the body after
        // the response headers have been sent.
        let pre = self.pre.clone();
        let task = tokio::spawn(async move {
            let proxy = pre.instantiate_async(&mut store).await?;
            proxy
                .wasi_http_incoming_handler()
                .call_handle(&mut store, req, out)
                .await
        });

        // The epoch deadline interrupts running wasm; this timeout also covers a
        // guest that is blocked waiting (e.g. sleeping) rather than running.
        let timeout = Timeout(self.limits.timeout);
        let Ok(response) = tokio::time::timeout(timeout.0, receiver).await else {
            task.abort();
            return Err(wasmtime::Error::new(timeout));
        };

        match response {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(code)) => bail!("function returned an error: {code:?}"),
            // The guest dropped the response handle without using it.
            Err(_) => match task.await {
                Ok(Ok(())) => bail!("function finished without sending a response"),
                Ok(Err(err)) if limits::is_interrupt(&err) => Err(wasmtime::Error::new(timeout)),
                Ok(Err(err)) => Err(err.context("function trapped")),
                Err(err) => Err(wasmtime::Error::from(err).context("function task panicked")),
            },
        }
    }
}

type OutgoingIoFuture = Box<dyn Future<Output = Result<(), HttpError>> + Send>;

/// Sends a guest's outgoing HTTP requests only to its allowed hosts.
pub(crate) struct OutboundPolicy(pub(crate) Arc<Settings>);

impl WasiHttpHooks for OutboundPolicy {
    fn send_request(
        &mut self,
        request: hyper::Request<WasiBody>,
        options: Option<RequestOptions>,
        _fut: OutgoingIoFuture,
    ) -> Box<
        dyn Future<Output = Result<(hyper::Response<WasiBody>, OutgoingIoFuture), HttpError>>
            + Send,
    > {
        let allowed = request
            .uri()
            .host()
            .is_some_and(|host| self.0.allows_host(host));
        if !allowed {
            return Box::new(async { Err(HttpError::HttpRequestDenied) });
        }
        Box::new(async move {
            let (response, io) = wasmtime_wasi_http::default_send_request(request, options).await?;
            Ok((
                response.map(BodyExt::boxed_unsync),
                Box::new(io) as OutgoingIoFuture,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn host_allowlist() {
        let settings = Settings {
            allowed_hosts: vec!["api.example.com".into(), "*.github.com".into()],
            ..Settings::default()
        };
        assert!(settings.allows_host("api.example.com"));
        assert!(settings.allows_host("API.Example.com"));
        assert!(settings.allows_host("raw.github.com"));
        assert!(!settings.allows_host("github.com"));
        assert!(!settings.allows_host("example.com"));
        assert!(!settings.allows_host("api.example.com.evil.net"));
        assert!(!Settings::default().allows_host("api.example.com"));
    }
}
